//! Nạp instruction/skill từ `prompts/*.md` lúc khởi động, thay vì hardcode trong code.
//! Sửa file rồi restart là đổi được tính cách, không cần build lại.

use std::path::PathBuf;

const FILES: [&str; 3] = ["persona.md", "lore.md", "rules.md"];

/// Trả về (system prompt, mô tả nguồn để log).
/// Thiếu cả 3 file thì dùng fallback nhúng trong binary.
pub fn load(fallback: &str) -> (String, String) {
    let dir = std::env::var("PROMPTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("prompts"));

    let mut parts = Vec::new();
    let mut used = Vec::new();
    for name in FILES {
        match std::fs::read_to_string(dir.join(name)) {
            Ok(text) if !text.trim().is_empty() => {
                parts.push(text.trim().to_string());
                used.push(name);
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        return (fallback.to_string(), format!("fallback nhúng (không thấy {})", dir.display()));
    }
    (
        parts.join("\n\n"),
        format!("{} từ {}", used.join(" + "), dir.display()),
    )
}
