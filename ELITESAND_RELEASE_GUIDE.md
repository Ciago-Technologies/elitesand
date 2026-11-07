# Elitesand branch-promotion, release, and package guide

This guide describes the repository's protected-branch workflow:

```text
feature branch -> dev -> staging -> prod -> main
```

The repository is `https://github.com/Ciago-Technologies/elitesand`. The default branch is
`main`. Direct pushes to `dev`, `staging`, `prod`, and `main` are protected and require a
pull request plus the required `build` status check.

## 1. What the release labels mean

This project has several independent version labels:

| Label                | Meaning                                                                                                                |
| -------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| `v0.1.1`             | Git tag and GitHub stable release                                                                                      |
| `0.1.1`              | GitHub Container Registry image tag                                                                                    |
| `v2`                 | Container image alias requested for the second package generation                                                      |
| `latest`             | Container image alias for the current published image                                                                  |
| `0.1.0` / `v0.1.0`   | Previous GitHub release, now deprecated                                                                                |
| Cargo crate versions | Source package versions in each crate's `Cargo.toml`; do not change them merely because a GitHub release label changes |

Do not call a release stable until the source commit, Git tag, GitHub release, container
image, and promotion PRs all point to the intended revision.

## 2. Prerequisites

Install and authenticate:

```bash
git --version
gh --version
docker --version
gh auth login
gh auth status
```

The GitHub account needs repository write access and `write:packages` permission. Confirm the
remote and clone the repository:

```bash
git clone https://github.com/Ciago-Technologies/elitesand.git
cd elitesand
git remote -v
```

Never put a GitHub token, private key, certificate key, or registry password in this guide,
the repository, a commit, or a shell history. Use `gh auth login` and `gh auth token` only
through stdin when Docker authentication is needed.

## 3. Start from any feature branch

Replace `feature/my-change` with the actual branch name. Fetch all remote refs before
comparing or rebasing:

```bash
git fetch --prune origin
git switch feature/my-change
git pull --ff-only origin feature/my-change
git status --short --branch
```

If the branch does not exist locally:

```bash
git switch --track -c feature/my-change origin/feature/my-change
```

Check that the working tree contains only intentional changes:

```bash
git status --short
git diff --check
git diff --stat
```

Commit the feature work before opening a PR:

```bash
git add path/to/intentional/file1 path/to/intentional/file2
git commit -m "feat: describe the change"
git push -u origin feature/my-change
```

Use explicit paths with `git add`; do not use `git add .` when unrelated or generated files
may be present.

## 4. Run the same checks used by CI

Run these commands from the repository root:

```bash
cargo fetch --locked
cargo fmt --all -- --check
python3 scripts/validate_workspace.py
python3 scripts/validate_documentation.py
cargo test --release --offline --workspace -- --nocapture
cargo test --release --offline --workspace --all-features -- --nocapture
cargo clippy --offline --workspace --all-targets --all-features
python3 scripts/check_mesh_progress.py
```

The GitHub workflow is [`.github/workflows/rust.yml`](elitesand/.github/workflows/rust.yml).
It also installs the pinned OpenSSL 3.5.9 provider before running the checks. A local pass
does not replace the required GitHub `build` check.

For the alpha benchmark, the current source uses four physical cores on the validated
host:

```bash
cargo run --release --offline -p ciagomem-engine --example alpha_benchmark
```

Record the output, including workers, CPU list, throughput, queue-wait P50/P95/P99,
service-time P50/P95/P99, errors, and the host conditions. Do not claim production
capacity from one noisy benchmark run.

## 5. Open the feature-to-dev pull request

Create the PR with `gh`:

```bash
gh pr create \
  --repo Ciago-Technologies/elitesand \
  --base dev \
  --head feature/my-change \
  --title "feat: promote my change to dev" \
  --body-file /path/to/pr-body.md
```

Or use an inline body for a small change:

```bash
gh pr create \
  --repo Ciago-Technologies/elitesand \
  --base dev \
  --head feature/my-change \
  --title "feat: promote my change to dev" \
  --body "Summary, validation commands, benchmark evidence, and known limitations."
```

Inspect the PR and required check:

```bash
gh pr view --repo Ciago-Technologies/elitesand --json number,url,state,headRefName,baseRefName,statusCheckRollup
gh pr checks PR_NUMBER --repo Ciago-Technologies/elitesand --watch
```

Merge only after review and a successful `build` check:

```bash
gh pr merge PR_NUMBER \
  --repo Ciago-Technologies/elitesand \
  --merge \
  --delete-branch=false
```

If branch protection rejects the merge, do not force-push. Resolve the requested review or
failed check, update the feature branch, and let the PR checks run again.

## 6. Promote through staging, prod, and main

After the dev PR merges, refresh local refs:

```bash
git fetch --prune origin
git switch dev
git pull --ff-only origin dev
```

Open the next promotion PR:

```bash
gh pr create \
  --repo Ciago-Technologies/elitesand \
  --base staging \
  --head dev \
  --title "release: promote dev to staging" \
  --body "Promotes the tested dev revision. Include test, security, and benchmark evidence."
```

Wait for review and checks, then merge:

```bash
gh pr checks PR_NUMBER --repo Ciago-Technologies/elitesand --watch
gh pr merge PR_NUMBER --repo Ciago-Technologies/elitesand --merge --delete-branch=false
```

Repeat the same sequence:

```bash
git fetch --prune origin

# staging -> prod
gh pr create \
  --repo Ciago-Technologies/elitesand \
  --base prod \
  --head staging \
  --title "release: promote staging to prod" \
  --body "Promotes the staging-validated revision to prod."

# prod -> main
gh pr create \
  --repo Ciago-Technologies/elitesand \
  --base main \
  --head prod \
  --title "release: promote prod to main" \
  --body "Promotes the production-validated revision to main."
```

For each PR:

1. Confirm the head and base are correct.
2. Read the changed-files tab and verify no accidental files are included.
3. Wait for the required `build` check.
4. Obtain required review approvals.
5. Merge using the repository's permitted merge method.
6. Fetch the resulting branch before opening the next promotion PR.

Do not open four PRs from one feature branch as a substitute for promotion order unless the
repository owner explicitly accepts that shortcut. Sequential promotion keeps each
environment's checks and approvals meaningful.

## 7. Prepare a release commit

Before tagging, ensure `main` contains the exact revision intended for release:

```bash
git fetch --prune origin
git switch main
git pull --ff-only origin main
git status --short --branch
git log -1 --oneline --decorate
```

Build the release server. The current binary target is `ciagomem-server`:

```bash
cargo build --release --offline -p ciagomem-engine --bin ciagomem-server
test -x target/release/ciagomem-server
```

The current Dockerfile copies that binary:

```dockerfile
COPY target/release/ciagomem-server /usr/local/bin/ciagomem-server
```

If the binary target or Dockerfile path changes, fix and test that mismatch before creating a
release. Do not silently publish an image that cannot start.

Run the release validation again:

```bash
cargo fmt --all -- --check
python3 scripts/validate_workspace.py
python3 scripts/validate_documentation.py
cargo test --release --offline --workspace
cargo test --release --offline --workspace --all-features
git diff --check
```

Commit only intentional release files:

```bash
git add Dockerfile GATE.md ciagomem-engine/examples/alpha_benchmark.rs
git commit -m "release: package stable 0.1.1" -m \
"Describe the release contents and validation.\

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

Push the feature/release branch first if the commit still needs promotion:

```bash
git push origin HEAD:feature/release-0.1.1
```

Then promote it through the protected branch PR sequence in Sections 5 and 6. Tag only the
final released commit on `main`.

## 8. Create the Git tag and GitHub release

Verify that the tag does not already exist:

```bash
git tag --list 'v0.1.1'
git ls-remote --tags origin 'refs/tags/v0.1.1'
```

Create and push an annotated tag:

```bash
git tag -a v0.1.1 -m "Stable release 0.1.1"
git push origin v0.1.1
```

Create the GitHub release:

```bash
gh release create v0.1.1 \
  --repo Ciago-Technologies/elitesand \
  --verify-tag \
  --title "0.1.1 Stable" \
  --notes-file /path/to/release-notes.md
```

Example release notes should state:

- the exact commit or tag;
- major code and documentation changes;
- validation commands and results;
- benchmark conditions and limitations;
- container image tags;
- known unsupported or unqualified capabilities.

To deprecate the previous release without deleting it:

```bash
gh release edit v0.1.0 \
  --repo Ciago-Technologies/elitesand \
  --title "0.1.0 (Deprecated)" \
  --notes "Deprecated; superseded by stable release 0.1.1."
```

Do not delete an old tag or release merely to mark it deprecated. Keeping it preserves
traceability for users who already consumed it.

## 9. Build and publish the container package

Build the server before building the image:

```bash
cargo build --release --offline -p ciagomem-engine --bin ciagom-server
```

Log in to GHCR without putting the token in the command line:

```bash
printf '%s\n' "$(gh auth token)" | \
  docker login ghcr.io -u YOUR_GITHUB_USERNAME --password-stdin
```

Use the lowercase repository path:

```bash
IMAGE=ghcr.io/ciago-technologies/elitesand
docker build \
  -t "$IMAGE:0.1.1" \
  -t "$IMAGE:v2" \
  -t "$IMAGE:latest" \
  .
```

Inspect the local image:

```bash
docker image inspect "$IMAGE:0.1.1" >/dev/null
docker run --rm "$IMAGE:0.1.1" --help
```

The service may require runtime TLS, identity, storage, and CPU-affinity configuration.
`--help` is only a startup/entrypoint check; it is not a production deployment test.

Push the immutable release tag first, then aliases:

```bash
docker push "$IMAGE:0.1.1"
docker push "$IMAGE:v2"
docker push "$IMAGE:latest"
```

Verify the registry tags:

```bash
gh api --paginate \
  /orgs/Ciago-Technologies/packages/container/elitesand/versions \
  --jq '.[] | {id: .id, tags: .metadata.container.tags, created_at: .created_at}'
```

If GHCR reports `Package not found`, push the first image with an account that has package
write permission or adjust the package visibility/permissions in GitHub. Do not publish a
second, similarly named package by accident.

## 10. Post-release verification

Confirm all release references:

```bash
git fetch --prune --tags origin
git ls-remote --heads origin
git ls-remote --tags origin 'refs/tags/v0.1.1'
gh release view v0.1.1 --repo Ciago-Technologies/elitesand
gh release view v0.1.0 --repo Ciago-Technologies/elitesand
gh run list --repo Ciago-Technologies/elitesand --limit 10
```

Check the protected branches:

```bash
for branch in dev staging prod main; do
  git ls-remote --heads origin "$branch"
done
```

Check the package tags:

```bash
docker pull ghcr.io/ciago-technologies/elitesand:0.1.1
docker pull ghcr.io/ciago-technologies/elitesand:v2
docker pull ghcr.io/ciago-technologies/elitesand:latest
```

Record the following in the release notes or internal change record:

- release tag and commit SHA;
- PR numbers for feature-to-dev and every promotion;
- CI run URLs and final conclusions;
- image digest for the immutable release image;
- benchmark output and host conditions;
- whether the old release was deprecated;
- any remaining production qualification gaps.

## 11. Troubleshooting

### Direct push is rejected

Expected error:

```text
Changes must be made through a pull request.
Required status check "build" is expected.
```

Create a PR to the target branch and wait for `build`. Never use `--force` to bypass branch
protection.

### The required check is pending

Inspect it:

```bash
gh pr checks PR_NUMBER --repo Ciago-Technologies/elitesand
gh run list --repo Ciago-Technologies/elitesand --branch BRANCH_NAME --limit 10
```

Open the failed job logs:

```bash
gh run view RUN_ID --repo Ciago-Technologies/elitesand --log-failed
```

Fix the source or workflow on the PR branch, push a new commit, and wait for a new check.

### The Docker build cannot find the server binary

Build the actual configured target and compare the paths:

```bash
cargo metadata --no-deps --format-version 1 | grep -n 'ciagom-server\|ciagomem-server'
cargo build --release --offline -p ciagomem-engine --bin ciagom-server
ls -l target/release/ciagom-server
grep '^COPY' Dockerfile
```

The `COPY` source and Cargo output must match exactly.

### The package tag is wrong

List local images:

```bash
docker image ls ghcr.io/ciago-technologies/elitesand
```

Retag the same tested image, then push the corrected tag. Do not rebuild from an unverified
working tree without recording the new digest.

### A tag already exists

Do not move a published release tag. Choose a new patch version, or explicitly coordinate a
release correction using a new tag. Rewriting release history breaks reproducibility.

### Benchmark results fluctuate

Record CPU affinity, workload, warm-up, duration, host load, VM/hypervisor activity,
thermal state, kernel, and exact commit. Report repeated runs and distributions rather than
one favorable run. A benchmark smoke result does not establish a production latency SLO,
million-user capacity, or power-loss durability.

## 12. Safe release checklist

- [ ] Feature branch is pushed and contains only intended files.
- [ ] Local formatting, validators, tests, and Clippy pass.
- [ ] Feature-to-dev PR is reviewed and `build` passes.
- [ ] Dev-to-staging PR is reviewed and merged.
- [ ] Staging-to-prod PR is reviewed and merged.
- [ ] Prod-to-main PR is reviewed and merged.
- [ ] Final `main` commit is verified.
- [ ] Release server binary builds.
- [ ] Docker image starts and `--help` succeeds.
- [ ] Annotated `v0.1.1` tag is pushed.
- [ ] GitHub `0.1.1 Stable` release is published.
- [ ] `v0.1.0` is marked deprecated, not deleted.
- [ ] GHCR tags `0.1.1`, `v2`, and `latest` are pushed.
- [ ] Image digest and CI results are recorded.
- [ ] Remaining qualification gaps are disclosed.
