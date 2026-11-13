//! Tìm kiếm web cho Reisa — không thêm dependency, không cần API key.
//!
//! ponytail: dùng nguồn keyless thay vì scrape DuckDuckGo/SearxNG. DDG trả HTTP 202
//! (trang challenge) cho fingerprint TLS của reqwest, còn SearxNG công khai chặn JSON.
//! Hiện dùng: Google News RSS (tin mới) + Wikipedia (nền tảng kiến thức).
//! Nâng cấp: cắm API trả JSON có key (Brave/Serper) khi cần phủ web rộng hơn.

const NEWS: &str = "https://news.google.com/rss/search?hl=vi&gl=VN&ceid=VN:vi&q=";
const WIKI: &str = "https://api.wikimedia.org/core/v1/wikipedia/";
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

pub const MAX_HITS: usize = 3;

pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Từ khoá khiến bot tự tra thông tin mới trước khi trả lời.
const TRIGGERS: [&str; 14] = [
    "tìm kiếm", "tra cứu", "search", "tin tức", "mới nhất", "hôm nay", "hôm qua",
    "thời tiết", "tỉ giá", "tỷ giá", "giá vàng", "bây giờ", "dạo này", "gần đây",
];

pub fn wants_search(text: &str) -> bool {
    let lower = text.to_lowercase();
    TRIGGERS.iter().any(|t| lower.contains(t))
}

/// Bỏ tiền tố "TênNgười: " trong prompt gộp để lấy câu truy vấn sạch.
pub fn query_from_prompt(prompt: &str) -> String {
    let last = prompt.lines().last().unwrap_or(prompt).trim();
    match last.split_once(": ") {
        Some((head, rest)) if head.chars().count() < 40 => rest.trim().to_string(),
        _ => last.to_string(),
    }
}

pub async fn search(http: &reqwest::Client, query: &str) -> Result<Vec<Hit>, String> {
    let mut hits = fetch_news(http, query).await.unwrap_or_default();
    if hits.len() < MAX_HITS {
        if let Ok(more) = fetch_wiki(http, query).await {
            hits.extend(more);
        }
    }
    hits.truncate(MAX_HITS);
    if hits.is_empty() {
        return Err(format!("không nguồn nào trả kết quả cho \"{query}\""));
    }
    Ok(hits)
}

async fn get(http: &reqwest::Client, url: &str) -> Result<String, String> {
    let response = http
        .get(url)
        .header("User-Agent", UA)
        .header("Accept", "application/json,text/xml,text/html;q=0.9,*/*;q=0.8")
        .header("Accept-Language", "vi,en;q=0.8")
        .send()
        .await
        .map_err(|e| format!("transport: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    response.text().await.map_err(|e| format!("body: {e}"))
}

async fn fetch_news(http: &reqwest::Client, query: &str) -> Result<Vec<Hit>, String> {
    let body = get(http, &format!("{NEWS}{}", url_encode(query))).await?;
    Ok(parse_rss(&body, MAX_HITS))
}

async fn fetch_wiki(http: &reqwest::Client, query: &str) -> Result<Vec<Hit>, String> {
    let url = format!("{WIKI}vi/search/page?q={}&limit=2", url_encode(query));
    let body = get(http, &url).await?;
    Ok(parse_wiki(&body))
}

/// Google News RSS: lấy `<item>` -> title/link/pubDate.
pub fn parse_rss(xml: &str, limit: usize) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut cursor = 0usize;
    while hits.len() < limit {
        let Some(start_rel) = xml[cursor..].find("<item>") else { break };
        let start = cursor + start_rel + "<item>".len();
        let Some(end_rel) = xml[start..].find("</item>") else { break };
        let item = &xml[start..start + end_rel];
        cursor = start + end_rel;

        let title = tag(item, "title").map(|t| clean(&t)).unwrap_or_default();
        let link = tag(item, "link").map(|t| clean(&t)).unwrap_or_default();
        let date = tag(item, "pubDate").map(|t| t.trim().to_string()).unwrap_or_default();
        if title.is_empty() || link.is_empty() {
            continue;
        }
        hits.push(Hit {
            title,
            url: link,
            snippet: if date.is_empty() { String::new() } else { format!("({date})") },
        });
    }
    hits
}

/// Wikimedia search API: `pages[]` có title/key/description/excerpt.
pub fn parse_wiki(json: &str) -> Vec<Hit> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(pages) = value.get("pages").and_then(|p| p.as_array()) else {
        return Vec::new();
    };
    pages
        .iter()
        .filter_map(|page| {
            let title = page.get("title")?.as_str()?.to_string();
            let key = page.get("key").and_then(|k| k.as_str()).unwrap_or_default();
            let snippet = page
                .get("description")
                .and_then(|d| d.as_str())
                .filter(|d| !d.is_empty())
                .map(|d| d.to_string())
                .or_else(|| {
                    page.get("excerpt")
                        .and_then(|e| e.as_str())
                        .map(|e| clean(&decode_entities(e)))
                })
                .unwrap_or_default();
            Some(Hit {
                title,
                url: format!("https://vi.wikipedia.org/wiki/{key}"),
                snippet,
            })
        })
        .collect()
}

pub fn format_hits(query: &str, hits: &[Hit]) -> String {
    let mut out = format!(
        "Dữ liệu mới nhất vừa tra cho \"{query}\" (chỉ dùng thông tin dưới đây cho phần cần dữ liệu thời sự, và nêu nguồn khi trích):"
    );
    for (i, hit) in hits.iter().enumerate() {
        out.push_str(&format!(
            "\n{}. {} {} — {}",
            i + 1,
            hit.title,
            hit.snippet,
            hit.url
        ));
    }
    out
}

/// Lấy text trong thẻ đầu tiên, có xử lý CDATA.
fn tag(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    let raw = xml[start..end].trim();
    let raw = raw
        .strip_prefix("<![CDATA[")
        .and_then(|r| r.strip_suffix("]]>"))
        .unwrap_or(raw);
    Some(decode_entities(raw))
}

fn clean(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    decode_entities(out.trim())
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&hellip;", "…")
}

pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push('+'),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0"?><rss><channel>
      <item><title>Blue Archive ra mắt sự kiện mới - GameK</title>
        <link>https://news.google.com/rss/articles/abc</link>
        <pubDate>Fri, 13 Nov 2026 04:00:00 GMT</pubDate></item>
      <item><title><![CDATA[Reisa &amp; Kazusa]]></title>
        <link>https://news.google.com/rss/articles/def</link>
        <pubDate>Thu, 12 Nov 2026 04:00:00 GMT</pubDate></item>
    </channel></rss>"#;

    #[test]
    fn parses_google_news_rss() {
        let hits = parse_rss(RSS, 3);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Blue Archive ra mắt sự kiện mới - GameK");
        assert_eq!(hits[0].url, "https://news.google.com/rss/articles/abc");
        assert!(hits[0].snippet.contains("2026"));
        assert_eq!(hits[1].title, "Reisa & Kazusa", "phải bóc được CDATA + entity");
    }

    const WIKI_JSON: &str = r#"{"pages":[
      {"id":1,"key":"Blue_Archive","title":"Blue Archive","description":"Trò chơi điện tử","excerpt":"<span class=\"searchmatch\">Blue</span> Archive là game"},
      {"id":2,"key":"Uzawa_Reisa","title":"Uzawa Reisa","description":"","excerpt":""}
    ]}"#;

    #[test]
    fn parses_wikimedia_search() {
        let hits = parse_wiki(WIKI_JSON);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://vi.wikipedia.org/wiki/Blue_Archive");
        assert_eq!(hits[0].snippet, "Trò chơi điện tử");
        assert_eq!(hits[1].snippet, "", "excerpt rỗng thì để trống");
    }

    #[test]
    fn detects_search_intent() {
        assert!(wants_search("tìm kiếm tin tức Blue Archive mới nhất"));
        assert!(wants_search("thời tiết hôm nay thế nào"));
        assert!(!wants_search("em khỏe không"));
    }

    #[test]
    fn extracts_query_from_grouped_prompt() {
        assert_eq!(
            query_from_prompt("vinh: tìm tin tức Blue Archive\nvinh: mới nhất hôm nay"),
            "mới nhất hôm nay"
        );
        assert_eq!(query_from_prompt("thời tiết hôm nay"), "thời tiết hôm nay");
    }

    #[test]
    fn encodes_query_for_url() {
        assert_eq!(url_encode("blue archive"), "blue+archive");
        assert_eq!(url_encode("giá vàng"), "gi%C3%A1+v%C3%A0ng");
    }

    #[test]
    fn formats_hits_with_sources() {
        let hits = vec![Hit {
            title: "T".into(),
            url: "https://x.test".into(),
            snippet: "S".into(),
        }];
        let text = format_hits("q", &hits);
        assert!(text.contains("https://x.test"));
        assert!(text.contains("1. T S"));
    }

    /// `cargo test live_search -- --ignored` — gọi mạng thật.
    #[tokio::test]
    #[ignore = "gọi mạng thật"]
    async fn live_search_returns_hits() {
        let http = reqwest::Client::new();
        let hits = search(&http, "Blue Archive").await.expect("search chạy được");
        assert!(!hits.is_empty(), "phải có kết quả");
        assert!(hits[0].url.starts_with("http"), "url: {}", hits[0].url);
        println!("{}", format_hits("Blue Archive", &hits));
    }
}
