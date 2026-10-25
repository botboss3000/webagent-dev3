#[test]
fn chat_module_compiles() {
    let _ = crate::configuration::AttachmentLimits {
        max_file_size_mb: Some(1),
        max_per_message: None,
    };
}
