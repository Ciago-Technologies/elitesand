//! Reviewer R6 probe: PASSES while the defect exists.
// ---------------------------------------------------------------------------
// R6-N5: image validation requires an exact epoch match while the log epoch is
// +1 per open. Without the (absent) trusted per-tenant epoch metadata, a
// caller passing the current epoch rejects every pre-restart image.
// ---------------------------------------------------------------------------
#[test]
fn n5_previous_epoch_loads_but_future_epoch_and_trusted_mismatch_reject() {
    use ciagomem_wal::envelope::ExtentImage;
    let img = ExtentImage {
        tenant: 7,
        incarnation: 8,
        tombstone: false,
        layout_version: 1,
        checkpoint_lsn: 10,
        epoch: 1,
        image_seq: 1,
    };
    let mut buf = vec![0u8; 4096];
    let w = img.header_words(&buf[128..]);
    for (i, x) in w.iter().enumerate() {
        buf[i * 8..i * 8 + 8].copy_from_slice(&x.to_le_bytes());
    }
    assert!(ExtentImage::validate(&buf, 7, 8, 1).is_some());
    assert!(
        ExtentImage::validate(&buf, 7, 8, 2).is_some(),
        "previous open remains valid"
    );
    assert!(ExtentImage::validate(&buf, 7, 8, 0).is_none());
    use ciagomem_wal::envelope::ImageStatus;
    assert_eq!(
        ExtentImage::inspect(&buf, 7, 8, 2, Some(2)),
        ImageStatus::Invalid
    );
    assert_eq!(
        ExtentImage::inspect(&[0; 4096], 7, 8, 2, None),
        ImageStatus::Absent
    );
    buf[0] ^= 1;
    assert_eq!(
        ExtentImage::inspect(&buf, 7, 8, 2, None),
        ImageStatus::Invalid
    );
}
