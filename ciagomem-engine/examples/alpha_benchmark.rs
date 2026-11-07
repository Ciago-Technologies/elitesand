//! Shared-nothing thread-per-core benchmark scaffold.
//!
//! Each worker owns its state and one io_uring instance. The only shared
//! objects are the bounded SPSC queues used to transfer ownership of tasks.
//! Run on Linux with:
//! `cargo run --release --offline -p ciagomem-engine --example alpha_benchmark`

#![cfg(target_os = "linux")]

use ciagomem_core::reactor;
use ciagomem_core::uring::UringIo;
use hdrhistogram::Histogram;
use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

// This host exposes four higher-frequency performance cores before its
// lower-frequency cores. Keep the alpha run on those four physical cores.
const TARGET_CORES: usize = 4;
const QUEUE_CAPACITY: usize = 4096;
const VECTOR_DIMENSIONS: usize = 384;
const WARMUP_DURATION: Duration = Duration::from_secs(30);
const MEASUREMENT_DURATION: Duration = Duration::from_secs(60);

struct SpscQueue<T, const N: usize> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

// The producer owns writes to slots and head; the consumer owns reads and tail.
// The atomics publish slot ownership before the other side accesses a slot.
unsafe impl<T: Send, const N: usize> Send for SpscQueue<T, N> {}
unsafe impl<T: Send, const N: usize> Sync for SpscQueue<T, N> {}

impl<T, const N: usize> SpscQueue<T, N> {
    fn new() -> Self {
        assert!(N.is_power_of_two());
        let slots = (0..N)
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            slots,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    fn try_push(&self, value: T) -> Result<(), T> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) == N {
            return Err(value);
        }

        let index = head & (N - 1);
        // SAFETY: only the single producer writes this slot, and the capacity
        // check proves the consumer has released its previous value.
        unsafe {
            (*self.slots[index].get()).write(value);
        }
        self.head.store(head.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    fn try_pop(&self) -> Option<T> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail == head {
            return None;
        }

        let index = tail & (N - 1);
        // SAFETY: only the single consumer reads this slot, and the acquire
        // load observes the producer's initialized value.
        let value = unsafe { (*self.slots[index].get()).assume_init_read() };
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(value)
    }
}

impl<T, const N: usize> Drop for SpscQueue<T, N> {
    fn drop(&mut self) {
        while self.try_pop().is_some() {}
    }
}

#[derive(Debug)]
enum Task {
    KvGet {
        key: u64,
        enqueued: Instant,
        measured: bool,
    },
    KvPut {
        key: u64,
        value: Box<[u8; 1024]>,
        enqueued: Instant,
        measured: bool,
    },
    VectorDot {
        lhs: Box<[f32; VECTOR_DIMENSIONS]>,
        rhs: Box<[f32; VECTOR_DIMENSIONS]>,
        enqueued: Instant,
        measured: bool,
    },
    RelationalLookup {
        key: u64,
        enqueued: Instant,
        measured: bool,
    },
    Stop,
}

impl Task {
    fn mark_enqueued(&mut self) {
        let now = Instant::now();
        match self {
            Self::KvGet { enqueued, .. }
            | Self::KvPut { enqueued, .. }
            | Self::VectorDot { enqueued, .. }
            | Self::RelationalLookup { enqueued, .. } => *enqueued = now,
            Self::Stop => {}
        }
    }
}

#[derive(Debug)]
struct WorkerStats {
    completed: u64,
    errors: u64,
    queue_wait: Histogram<u64>,
    service_time: Histogram<u64>,
    vector_checksum: f32,
    io_ring_completions: usize,
}

impl Default for WorkerStats {
    fn default() -> Self {
        Self {
            completed: 0,
            errors: 0,
            queue_wait: Histogram::new(3).expect("valid queue-wait histogram"),
            service_time: Histogram::new(3).expect("valid service-time histogram"),
            vector_checksum: 0.0,
            io_ring_completions: 0,
        }
    }
}

struct WorkerState {
    kv_get: HashMap<u64, [u8; 64]>,
    kv_put: HashMap<u64, Box<[u8; 1024]>>,
    relational_rows: [u64; 1024],
}

impl WorkerState {
    fn new() -> Self {
        let kv_get = (0..QUEUE_CAPACITY as u64)
            .map(|key| (key, [key as u8; 64]))
            .collect::<HashMap<_, _>>();
        let kv_put = (0..QUEUE_CAPACITY as u64)
            .map(|key| (key, Box::new([key as u8; 1024])))
            .collect::<HashMap<_, _>>();
        Self {
            kv_get,
            kv_put,
            relational_rows: std::array::from_fn(|row| row as u64),
        }
    }
}

fn worker(
    worker_id: usize,
    cpu: usize,
    queue: Arc<SpscQueue<Task, QUEUE_CAPACITY>>,
) -> Result<WorkerStats, String> {
    reactor::pin(cpu).map_err(|error| format!("worker {worker_id} CPU {cpu}: {error}"))?;

    let mut io = UringIo::new(QUEUE_CAPACITY as u32)
        .map_err(|error| format!("worker {worker_id} io_uring: {error}"))?;
    let mut state = WorkerState::new();
    let mut stats = WorkerStats::default();
    loop {
        let task = loop {
            if let Some(task) = queue.try_pop() {
                break task;
            }
            thread::yield_now();
        };

        match task {
            Task::KvGet {
                key,
                enqueued,
                measured,
            } => {
                record_queue_wait(&mut stats, enqueued, measured);
                let service_started = Instant::now();
                if state.kv_get.get(&key).is_none() {
                    stats.errors += 1;
                }
                record_service_time(&mut stats, service_started, measured);
            }
            Task::KvPut {
                key,
                value,
                enqueued,
                measured,
            } => {
                record_queue_wait(&mut stats, enqueued, measured);
                let service_started = Instant::now();
                state.kv_put.insert(key, value);
                record_service_time(&mut stats, service_started, measured);
            }
            Task::VectorDot {
                lhs,
                rhs,
                enqueued,
                measured,
            } => {
                record_queue_wait(&mut stats, enqueued, measured);
                let service_started = Instant::now();
                let checksum = dot_product_avx2(&lhs, &rhs);
                if !checksum.is_finite() {
                    stats.errors += 1;
                }
                stats.vector_checksum += checksum;
                record_service_time(&mut stats, service_started, measured);
            }
            Task::RelationalLookup {
                key,
                enqueued,
                measured,
            } => {
                record_queue_wait(&mut stats, enqueued, measured);
                let service_started = Instant::now();
                if state.relational_rows[key as usize % state.relational_rows.len()] != key % 1024 {
                    stats.errors += 1;
                }
                record_service_time(&mut stats, service_started, measured);
            }
            Task::Stop => {
                reap_io_errors(&mut io, &mut stats);
                return Ok(stats);
            }
        }

        stats.completed += 1;
        // Keep the io_uring owner active without moving it across threads.
        reap_io_errors(&mut io, &mut stats);
    }
}

fn reap_io_errors(io: &mut UringIo, stats: &mut WorkerStats) {
    let mut errors = 0;
    let completions = io.reap(|_, result| {
        if result < 0 {
            errors += 1;
        }
    });
    stats.errors += errors;
    stats.io_ring_completions += completions;
}

fn record_queue_wait(stats: &mut WorkerStats, enqueued: Instant, measured: bool) {
    if measured {
        stats
            .queue_wait
            .record(enqueued.elapsed().as_nanos().min(u64::MAX as u128) as u64)
            .expect("queue-wait histogram range must be sufficient");
    }
}

fn record_service_time(stats: &mut WorkerStats, started: Instant, measured: bool) {
    if measured {
        stats
            .service_time
            .record(started.elapsed().as_nanos().min(u64::MAX as u128) as u64)
            .expect("service-time histogram range must be sufficient");
    }
}

#[target_feature(enable = "avx2")]
unsafe fn dot_product_avx2_impl(
    lhs: &[f32; VECTOR_DIMENSIONS],
    rhs: &[f32; VECTOR_DIMENSIONS],
) -> f32 {
    use std::arch::x86_64::{
        __m256, _mm256_add_ps, _mm256_loadu_ps, _mm256_mul_ps, _mm256_setzero_ps, _mm256_storeu_ps,
    };

    let mut sum: __m256 = _mm256_setzero_ps();
    for offset in (0..VECTOR_DIMENSIONS).step_by(8) {
        let left = _mm256_loadu_ps(lhs.as_ptr().add(offset));
        let right = _mm256_loadu_ps(rhs.as_ptr().add(offset));
        sum = _mm256_add_ps(sum, _mm256_mul_ps(left, right));
    }
    let mut lanes = [0.0_f32; 8];
    _mm256_storeu_ps(lanes.as_mut_ptr(), sum);
    lanes.into_iter().sum()
}

#[inline]
fn dot_product_avx2(lhs: &[f32; VECTOR_DIMENSIONS], rhs: &[f32; VECTOR_DIMENSIONS]) -> f32 {
    // The host check in main makes this guarded call valid for the benchmark.
    unsafe { dot_product_avx2_impl(lhs, rhs) }
}

fn physical_cpus() -> Result<Vec<usize>, String> {
    let allowed = reactor::allowed_cpus().map_err(|error| format!("CPU discovery: {error}"))?;
    let mut seen = Vec::new();
    let mut cpus = Vec::new();

    for cpu in allowed {
        let topology = format!("/sys/devices/system/cpu/cpu{cpu}/topology");
        let package = std::fs::read_to_string(format!("{topology}/physical_package_id"))
            .map_err(|error| format!("read physical package for CPU {cpu}: {error}"))?;
        let core = std::fs::read_to_string(format!("{topology}/core_id"))
            .map_err(|error| format!("read core id for CPU {cpu}: {error}"))?;
        let identity = (package.trim().to_owned(), core.trim().to_owned());
        if !seen.contains(&identity) {
            seen.push(identity);
            cpus.push(cpu);
        }
        if cpus.len() == TARGET_CORES {
            break;
        }
    }

    if cpus.len() < TARGET_CORES {
        return Err(format!(
            "need {TARGET_CORES} distinct physical cores, found {}",
            cpus.len()
        ));
    }
    Ok(cpus)
}

fn push(queue: &SpscQueue<Task, QUEUE_CAPACITY>, mut task: Task) -> Result<(), String> {
    loop {
        task.mark_enqueued();
        match queue.try_push(task) {
            Ok(()) => return Ok(()),
            Err(returned) => {
                task = returned;
                thread::yield_now();
            }
        }
    }
}

fn wait_for_workers(startup: &[Arc<WorkerStartup>], timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(error) = startup.iter().find_map(|status| status.error()) {
            return Err(error);
        }
        if startup
            .iter()
            .all(|status| status.ready.load(Ordering::Acquire))
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("worker startup timed out".to_owned());
        }
        thread::yield_now();
    }
}

struct WorkerStartup {
    ready: AtomicBool,
    failure: OnceLock<String>,
}

impl WorkerStartup {
    fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            failure: OnceLock::new(),
        }
    }

    fn error(&self) -> Option<String> {
        self.failure.get().cloned()
    }
}

fn make_vector(seed: f32) -> Box<[f32; VECTOR_DIMENSIONS]> {
    Box::new(std::array::from_fn(|index| seed + index as f32 * 0.001))
}

fn make_put_value(seed: u8) -> Box<[u8; 1024]> {
    Box::new([seed; 1024])
}

fn task_for(task_id: usize, measured: bool) -> Task {
    let enqueued = Instant::now();
    match task_id % 10 {
        0..=4 => Task::KvGet {
            key: task_id as u64 % QUEUE_CAPACITY as u64,
            enqueued,
            measured,
        },
        5..=6 => Task::KvPut {
            key: task_id as u64 % QUEUE_CAPACITY as u64,
            value: make_put_value(task_id as u8),
            enqueued,
            measured,
        },
        7..=8 => Task::VectorDot {
            lhs: make_vector(1.0),
            rhs: make_vector(2.0),
            enqueued,
            measured,
        },
        _ => Task::RelationalLookup {
            key: task_id as u64,
            enqueued,
            measured,
        },
    }
}

fn main() -> Result<(), String> {
    assert!(
        std::is_x86_feature_detected!("avx2"),
        "FATAL: AVX2 is required by alpha_benchmark"
    );
    assert!(
        UringIo::new(1).is_ok(),
        "FATAL: io_uring is required by alpha_benchmark"
    );
    let cpus = physical_cpus()?;
    let queues = (0..TARGET_CORES)
        .map(|_| Arc::new(SpscQueue::new()))
        .collect::<Vec<_>>();
    let startup = (0..TARGET_CORES)
        .map(|_| Arc::new(WorkerStartup::new()))
        .collect::<Vec<_>>();
    let mut workers = Vec::with_capacity(TARGET_CORES);

    for (worker_id, ((cpu, queue), status)) in
        cpus.iter().copied().zip(&queues).zip(&startup).enumerate()
    {
        let queue = Arc::clone(queue);
        let status = Arc::clone(status);
        workers.push(
            thread::Builder::new()
                .name(format!("elite-actor-{worker_id}"))
                .spawn(move || {
                    if let Err(error) = reactor::pin(cpu)
                        .and_then(|_| {
                            UringIo::new(QUEUE_CAPACITY as u32)
                                .map(|_| ())
                                .map_err(|error| std::io::Error::other(error.to_string()))
                        })
                        .map_err(|error| format!("worker {worker_id} startup: {error}"))
                    {
                        let _ = status.failure.set(error);
                        return Err("worker startup failed".to_owned());
                    }
                    status.ready.store(true, Ordering::Release);
                    worker(worker_id, cpu, queue)
                })
                .map_err(|error| format!("spawn worker {worker_id}: {error}"))?,
        );
    }

    wait_for_workers(&startup, Duration::from_secs(5))?;
    let warmup_deadline = Instant::now() + WARMUP_DURATION;
    let mut task_id = 0;
    while Instant::now() < warmup_deadline {
        for _ in 0..10 {
            push(&queues[task_id % TARGET_CORES], task_for(task_id, false))?;
            task_id += 1;
        }
    }
    let start = Instant::now();
    let deadline = start + MEASUREMENT_DURATION;
    let mut measured_submitted = 0;
    while Instant::now() < deadline {
        for _ in 0..10 {
            push(&queues[task_id % TARGET_CORES], task_for(task_id, true))?;
            task_id += 1;
            measured_submitted += 1;
        }
    }
    let measurement_elapsed = start.elapsed();

    for queue in &queues {
        push(queue, Task::Stop)?;
    }

    let mut totals = WorkerStats::default();
    for worker in workers {
        let stats = worker
            .join()
            .map_err(|_| "worker panicked".to_owned())?
            .map_err(|error| error)?;
        totals.completed += stats.completed;
        totals.errors += stats.errors;
        totals
            .queue_wait
            .add(&stats.queue_wait)
            .map_err(|_| "queue-wait histogram configuration mismatch".to_owned())?;
        totals
            .service_time
            .add(&stats.service_time)
            .map_err(|_| "service-time histogram configuration mismatch".to_owned())?;
        totals.vector_checksum += stats.vector_checksum;
        totals.io_ring_completions += stats.io_ring_completions;
    }

    let throughput = totals.service_time.len() as f64 / measurement_elapsed.as_secs_f64();
    println!(
        "alpha benchmark complete\n\
workers={} cpus={:?} submitted={} completed={} errors={} throughput_ops_sec={throughput:.2}\n\
queue_wait_ns: p50={} p95={} p99={}\n\
service_time_ns: p50={} p95={} p99={}\n\
io_ring_completions={} vector_checksum={:.3}",
        TARGET_CORES,
        cpus,
        measured_submitted,
        totals.completed,
        totals.errors,
        totals.queue_wait.value_at_quantile(0.50),
        totals.queue_wait.value_at_quantile(0.95),
        totals.queue_wait.value_at_quantile(0.99),
        totals.service_time.value_at_quantile(0.50),
        totals.service_time.value_at_quantile(0.95),
        totals.service_time.value_at_quantile(0.99),
        totals.io_ring_completions,
        totals.vector_checksum
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("alpha_benchmark requires Linux io_uring and CPU affinity support");
    std::process::exit(2);
}
