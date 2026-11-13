# Recon: bật web search trên ninerouter

Kết luận: **không có shape nào trong chat/completions bật được search thật.** Bot tự tra web
ở tầng ứng dụng (`src/websearch.rs`).

## 1. Thử bật search bằng tham số request

Model `ag/gemini-3.8-flash` (GET /v1/models quảng cáo `"search": true`).
Câu hỏi kiểm tra: "Hôm nay là ngày mấy? Trả lời ngắn gọn và nêu nguồn nếu có."
Nếu search bật thật thì mọi shape phải trả **cùng một ngày đúng**.

| Shape | Chạy? | Trả lời | Kết luận |
|---|---|---|---|
| baseline (không tham số) | HTTP 200 | "Không truy cập được thời gian thực... Nguồn: Giới hạn hệ thống AI offline." | không search |
| `{"search": true}` | 200 | "Không thể truy cập đồng hồ thời gian thực." | không search |
| `{"tools":[{"type":"web_search"}]}` | 200 | "Không truy cập thời gian thực." | không search |
| `{"tools":[{"type":"web_search_preview"}]}` | 200 | "Thiếu quyền truy cập đồng hồ hệ thống và Internet." | không search |
| `{"plugins":[{"id":"web"}]}` | 200 | "Hôm nay: 24/03/2026." | **bịa** |
| `{"web_search_options":{}}` | 200 | "Hôm nay: 30/03/2026." | **bịa** |
| `{"google_search":true}` | 200 | "Hôm nay: 31/03/2026." | **bịa** |
| `{"extra_body":{"google":{"search":true}}}` | 200 | "Không xác định được thời gian thực." | không search |

Bằng chứng quan trọng: ba shape "có vẻ chạy" trả **ba ngày khác nhau** cho cùng một câu hỏi
(24/03, 30/03, 31/03) — đó là model đoán, không phải dữ liệu thật. Không response nào chứa
`annotations`, `citations`, `grounding_metadata`, `url_citation`, `search_results`.

## 2. Thử scrape công cụ tìm kiếm (đều thất bại từ reqwest)

| Nguồn | Kết quả từ `curl` | Kết quả từ reqwest (rustls) |
|---|---|---|
| `lite.duckduckgo.com/lite/?q=` | 200, có `result-link` | **202 + trang challenge**, 0 kết quả |
| `html.duckduckgo.com/html/?q=` | 200, 10 `result__a` | **202 + trang challenge**, 0 kết quả |
| `priv.au/search?...&format=json` (SearxNG) | 429 | — |
| `searx.be`, `search.inetol.net` | 200 nhưng là HTML/security check | — |
| `r.jina.ai` | — | 403 |

DDG chặn theo fingerprint TLS/HTTP2 của reqwest, không phải theo User-Agent: cùng một UA,
curl qua được còn reqwest thì không.

## 3. Nguồn keyless dùng được từ reqwest (đã kiểm chứng)

| Nguồn | Endpoint | Kết quả |
|---|---|---|
| Google News RSS | `news.google.com/rss/search?hl=vi&gl=VN&ceid=VN:vi&q=` | 200, 60 KB RSS, có `<item>` |
| Wikimedia search | `api.wikimedia.org/core/v1/wikipedia/vi/search/page?q=&limit=2` | 200, JSON `pages[].title/key/description/excerpt` |
| Wikipedia API cũ | `en.wikipedia.org/w/api.php?action=query&list=search` | 200 JSON |
| DuckDuckGo Instant Answer | `api.duckduckgo.com/?q=&format=json` | 200 nhưng `Abstract` rỗng với truy vấn thường |
| Marginalia | `search.marginalia.nu/search?query=` | 200 HTML |
| Open-Meteo | `api.open-meteo.com/v1/forecast?...&current=temperature_2m` | 200 JSON (thời tiết) |
| Tỉ giá | `open.er-api.com/v6/latest/USD` | 200 JSON |

## 4. Cách đang dùng trong bot

`src/websearch.rs`: Google News RSS (tin thời sự, kèm `pubDate` + link) là nguồn chính,
Wikimedia là nguồn bổ sung khi RSS không đủ 3 kết quả. Kích hoạt bằng `/search <từ khoá>`
hoặc tự động khi tin nhắn chứa từ khoá ("tin tức", "mới nhất", "hôm nay", "thời tiết", ...).

Nâng cấp khi cần phủ web rộng hơn: cắm API trả JSON có key (Brave Search / Serper / Tavily)
— lúc đó chỉ cần thêm một hàm `fetch_*` và đổi thứ tự ưu tiên trong `search()`.
