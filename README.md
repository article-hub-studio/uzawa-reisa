# uzawa-reisa

Bot Discord viết bằng Rust (serenity + rustls), chạy dạng **background worker** trên Render: luôn mở kết nối gateway, không cần HTTP listener, không cần health check.

## Biến môi trường

| Tên | Bắt buộc | Ghi chú |
| --- | --- | --- |
| `DISCORD_BOT_TOKEN` | có | Token bot Discord — secret, điền trên Render dashboard |
| `NINEROUTER_API_KEY` | có | API key 9Router — secret, điền trên Render dashboard |
| `NINEROUTER_BASE_URL` | có | `https://ninerouterhoang.onrender.com/v1` |
| `NINEROUTER_MODEL` | có | `openrouter/openrouter/free` |

## Chạy local

```bash
export DISCORD_BOT_TOKEN=...        # bắt buộc
export NINEROUTER_API_KEY=...
export NINEROUTER_BASE_URL=https://ninerouterhoang.onrender.com/v1
export NINEROUTER_MODEL=openrouter/openrouter/free
cargo run --release
```

## Deploy lên Render

1. Push repo lên GitHub/GitLab.
2. Render dashboard → **New → Blueprint** → chọn repo. Render đọc `render.yaml` và tạo worker `uzawa-reisa` (Docker, region Singapore, plan starter).
3. Mở worker → **Environment** → điền `DISCORD_BOT_TOKEN` và `NINEROUTER_API_KEY`. Hai biến này khai báo `sync: false` nên Render sẽ hỏi giá trị khi tạo Blueprint và không bao giờ ghi vào git.
4. Apply → Render build từ `Dockerfile` và chạy container dưới user không phải root.
5. Xem log ở tab **Logs**. Worker là tiến trình chạy dài, không có port và không có health check.

Lưu ý: image build bằng `cargo build --release --locked`, nên sau khi sửa `Cargo.toml` phải commit cả `Cargo.lock` đã cập nhật.

Chi tiết spec: https://render.com/docs/blueprint-spec