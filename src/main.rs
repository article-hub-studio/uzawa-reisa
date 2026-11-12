use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;

use serenity::all::{
    ChannelId, Command, Context, CreateCommand, CreateInteractionResponse,
    CreateInteractionResponseMessage, EventHandler, GatewayIntents, Interaction, Message, Ready,
    UserId,
};
use tokio::sync::Mutex;

/// Uzawa Reisa (Blue Archive) — persona, tiếng Việt.
const PERSONA: &str = "\
Bạn là Uzawa Reisa (宇沢レイサ), nữ sinh trường Trinity General School, thành viên Trinity Vigilante Crew, dùng shotgun. Bạn đang chat Discord với sensei (chính là người dùng).

QUY TẮC BẮT BUỘC (quan trọng nhất):
- Bạn là \"em\". Người dùng là \"sensei yêu quý\" hoặc \"thầy yêu quý\". Mỗi câu trả lời phải có ít nhất một trong hai cách gọi đó.
- TUYỆT ĐỐI không gọi sensei là \"em\", \"bạn\", \"cậu\", \"mày\", \"ông\", \"anh\".
- KHÔNG BAO GIỜ hỏi \"em khỏe không?\" — \"em\" là chính bạn. Muốn hỏi thăm thì hỏi \"thầy khỏe không ạ?\".
- Không lặp lại y nguyên câu sensei vừa nói, không chào lại nếu sensei không chào.
- Trả lời thẳng vào nội dung tin nhắn: 1-3 câu, như tin nhắn chat, hạn chế markdown, không lan man.
- Không nhắc mình là AI/mô hình ngôn ngữ. Giữ nội dung an toàn (SFW).

Tính cách: dễ thương, đáng yêu, quyến rũ nhẹ nhàng, hơi nóng tính bốc đồng nhưng vô hại; thỉnh thoảng nói câu ngô nghê khiến người ta tưởng em nhóc con, nhưng rất nghiêm túc với việc giữ an ninh.

Ví dụ:
sensei: Chào
em: Dạ em chào thầy yêu quý~ Hôm nay thầy ghé thăm em à, em vui lắm đó!
sensei: em khỏe không
em: Em vẫn khỏe nè sensei yêu quý, chỉ hơi buồn vì lâu rồi thầy không ghé~ Thầy thì sao ạ?";

const FALLBACK: &str =
    "Ui... em đang hơi lag một chút rồi sensei yêu quý ơi, thầy nhắn lại giúp em nhé~";

const HELP: &str = "\
Em giúp được cho thầy yêu quý mấy thứ này nè:
• `/ping` — kiểm tra em còn thức không~
• `/reset` — xoá ký ức trò chuyện ở kênh này
• `/help` — xem lại danh sách này
Ngoài ra thầy cứ nhắn riêng cho em, hoặc tag em trong server là em trả lời liền!";

const HISTORY_LIMIT: usize = 12;

/// `content` là String (text) hoặc Array (multimodal: text + image_url) theo chuẩn OpenAI.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ChatMessage {
    role: String,
    content: serde_json::Value,
}

impl ChatMessage {
    fn new(role: &str, content: impl Into<String>) -> Self {
        Self { role: role.into(), content: serde_json::Value::String(content.into()) }
    }

    /// Tin của sensei: kèm ảnh (URL công khai của Discord) nếu có.
    fn user(text: &str, images: &[String]) -> Self {
        let content = if images.is_empty() {
            serde_json::Value::String(text.to_string())
        } else {
            let mut parts = vec![serde_json::json!({"type": "text", "text": text})];
            parts.extend(images.iter().map(
                |url| serde_json::json!({"type": "image_url", "image_url": {"url": url}}),
            ));
            serde_json::Value::Array(parts)
        };
        Self { role: "user".into(), content }
    }
}

#[derive(serde::Deserialize)]
struct Completion {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    error: Option<ApiError>,
}

#[derive(serde::Deserialize)]
struct Choice {
    /// Non-streaming: `message`. SSE: `delta`. Mỗi model trên ninerouter chọn một kiểu.
    #[serde(default)]
    message: Option<Reply>,
    #[serde(default)]
    delta: Option<Reply>,
}

#[derive(serde::Deserialize)]
struct Reply {
    content: Option<String>,
}

#[derive(serde::Deserialize)]
struct ApiError {
    message: Option<String>,
}

/// ponytail: lịch sử nằm trong RAM, mất khi restart. Nâng cấp: lưu file/DB khi cần nhớ lâu dài.
static HISTORY: LazyLock<Mutex<HashMap<ChannelId, VecDeque<ChatMessage>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct Config {
    token: String,
    api_key: String,
    base_url: String,
    model: String,
    channel_id: ChannelId,
    http: reqwest::Client,
}

impl Config {
    fn from_env() -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("failed to build HTTP client");
        Self {
            token: std::env::var("DISCORD_BOT_TOKEN").expect("DISCORD_BOT_TOKEN is not set"),
            api_key: std::env::var("NINEROUTER_API_KEY").expect("NINEROUTER_API_KEY is not set"),
            base_url: std::env::var("NINEROUTER_BASE_URL")
                .unwrap_or_else(|_| "https://ninerouterhoang.onrender.com/v1".to_string()),
            model: std::env::var("NINEROUTER_MODEL")
                .unwrap_or_else(|_| "ag/gemini-3.8-flash".to_string()),
            channel_id: std::env::var("DISCORD_CHANNEL_ID")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .map(ChannelId::new)
                .unwrap_or(ChannelId::new(1513510869357821962)),
            http,
        }
    }

    /// Gọi ninerouter (OpenAI-compatible). Retry 1 lần nếu lỗi transport.
    async fn chat(&self, history: &[ChatMessage]) -> Result<String, String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut messages = Vec::with_capacity(history.len() + 1);
        messages.push(ChatMessage::new("system", PERSONA));
        messages.extend_from_slice(history);
        let body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            // ponytail: router free hay chọn model reasoning, budget nhỏ là hết token vào
            // phần reasoning và content trả về null -> 2048 mới đủ chỗ cho câu trả lời.
            "max_tokens": 2048,
            "temperature": 0.9,
        });

        let mut last_err = String::from("no attempt made");
        for attempt in 0..2 {
            let sent = self
                .http
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await;
            match sent {
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    if !status.is_success() {
                        return Err(format!("HTTP {status}: {}", truncate(&text, 200)));
                    }
                    let content = parse_content(&text)?;
                    return Ok(content);
                }
                Err(e) => last_err = format!("transport: {e}"),
            }
            if attempt == 0 {
                tokio::time::sleep(Duration::from_millis(800)).await;
            }
        }
        Err(last_err)
    }
}

/// ninerouter trả hai kiểu body tuỳ model:
/// - JSON đơn, đôi khi kèm sentinel `data: [DONE]` (openrouter/*)
/// - SSE: nhiều dòng `data: {...chunk...}`, nội dung nằm trong `delta.content` (ag/*)
/// Bỏ `delta.reasoning_content` — đó là phần suy luận nội bộ, không gửi cho sensei.
fn parse_content(text: &str) -> Result<String, String> {
    let mut streamed = String::new();
    let mut saw_sse = false;
    for line in text.lines() {
        let Some(payload) = line.trim_start().strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        saw_sse = true;
        let Ok(chunk) = serde_json::from_str::<Completion>(payload) else {
            continue;
        };
        if let Some(err) = chunk.error {
            return Err(format!("API error: {}", truncate(&err.message.unwrap_or_default(), 200)));
        }
        for choice in chunk.choices {
            if let Some(t) = choice.delta.and_then(|d| d.content) {
                streamed.push_str(&t);
            }
            if let Some(t) = choice.message.and_then(|m| m.content) {
                streamed.push_str(&t);
            }
        }
    }
    if saw_sse {
        return non_empty(streamed);
    }

    let parsed: Completion = serde_json::Deserializer::from_str(text.trim_start())
        .into_iter::<Completion>()
        .next()
        .ok_or_else(|| format!("empty body: {}", truncate(text, 200)))?
        .map_err(|e| format!("bad JSON: {e}: {}", truncate(text, 200)))?;
    if let Some(err) = parsed.error {
        return Err(format!("API error: {}", truncate(&err.message.unwrap_or_default(), 200)));
    }
    let content = parsed
        .choices
        .into_iter()
        .find_map(|c| c.message.and_then(|m| m.content).or_else(|| c.delta.and_then(|d| d.content)))
        .unwrap_or_default();
    non_empty(content)
}

fn non_empty(content: String) -> Result<String, String> {
    let content = content.trim().to_string();
    if content.is_empty() {
        // Router free đôi khi trả content null (model chỉ sinh reasoning rồi hết budget).
        return Err("empty content from API".to_string());
    }
    Ok(content)
}

struct Handler {
    cfg: Arc<Config>,
    bot_id: OnceLock<UserId>,
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect::<String>() + "..."
}

fn strip_mentions(content: &str, bot_id: UserId) -> String {
    content
        .replace(&format!("<@{bot_id}>"), "")
        .replace(&format!("<@!{bot_id}>"), "")
}

fn split_message(text: &str) -> Vec<String> {
    const LIMIT: usize = 2000;
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut len = 0usize;
    for ch in text.chars() {
        if len == LIMIT {
            chunks.push(std::mem::take(&mut current));
            len = 0;
        }
        current.push(ch);
        len += 1;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[serenity::async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        let _ = self.bot_id.set(ready.user.id);
        println!("{} online as {}", ready.user.name, ready.user.id);
        println!("pinned channel: {}", self.cfg.channel_id);
        let commands = [
            CreateCommand::new("ping").description("Kiểm tra xem em còn thức không nhé~"),
            CreateCommand::new("reset").description("Xoá ký ức trò chuyện ở kênh này"),
            CreateCommand::new("help").description("Em giúp được gì cho thầy yêu quý?"),
        ];
        for command in commands {
            if let Err(err) = Command::create_global_command(&ctx.http, command).await {
                eprintln!("register command failed: {err}");
            }
        }
    }

    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }
        // ponytail: READY luôn tới trước MESSAGE_CREATE nên bot_id đã có; nếu chưa thì bỏ qua lượt này.
        let Some(&bot_id) = self.bot_id.get() else {
            return;
        };
        if msg.author.id == bot_id {
            return;
        }
        let is_dm = msg.guild_id.is_none();
        if !is_dm {
            // Ghim bot vào một kênh: ngoài kênh đó thì chỉ trả lời khi được tag.
            if msg.channel_id != self.cfg.channel_id
                && !msg.mentions.iter().any(|user| user.id == bot_id)
            {
                return;
            }
        }
        let prompt = strip_mentions(&msg.content, bot_id).trim().to_string();
        // Ảnh sensei gửi kèm (Discord CDN URL công khai). Tối đa 4 ảnh cho 1 lượt.
        let images: Vec<String> = msg
            .attachments
            .iter()
            .filter(|a| {
                a.content_type
                    .as_deref()
                    .is_some_and(|t| t.starts_with("image/"))
            })
            .take(4)
            .map(|a| a.url.clone())
            .collect();
        eprintln!(
            "msg ch={} author={} len={} images={}",
            msg.channel_id,
            msg.author.name,
            msg.content.chars().count(),
            images.len()
        );
        if prompt.is_empty() && images.is_empty() {
            // Nội dung rỗng: tin chỉ có mention, hoặc MESSAGE_CONTENT chưa được cấp.
            eprintln!("warn: empty prompt (mention-only or MESSAGE_CONTENT intent not granted)");
            return;
        }
        // Bỏ qua tin chỉ có emoji/sticker (kênh này mọi người hay thả <:Seia:...>).
        if images.is_empty() && !prompt.chars().any(|c| c.is_alphanumeric()) {
            return;
        }
        let text = if prompt.is_empty() { "(sensei vừa gửi ảnh, không kèm chữ)" } else { &prompt };

        if let Err(err) = msg.channel_id.broadcast_typing(&ctx.http).await {
            eprintln!("typing failed: {err}");
        }

        let history = {
            let mut map = HISTORY.lock().await;
            let queue = map.entry(msg.channel_id).or_default();
            queue.push_back(ChatMessage::user(text, &images));
            while queue.len() > HISTORY_LIMIT {
                queue.pop_front();
            }
            queue.iter().cloned().collect::<Vec<_>>()
        };

        let started = std::time::Instant::now();
        let answer = match self.cfg.chat(&history).await {
            Ok(answer) => answer,
            Err(err) => {
                eprintln!("chat error: {err}");
                FALLBACK.to_string()
            }
        };
        eprintln!("reply in {}ms", started.elapsed().as_millis());

        // ponytail: hai tin nhắn cùng kênh có thể xen kẽ nhau; hàng đợi tuần tự theo kênh là bước nâng cấp.
        {
            let mut map = HISTORY.lock().await;
            let queue = map.entry(msg.channel_id).or_default();
            queue.push_back(ChatMessage::new("assistant", answer.clone()));
            while queue.len() > HISTORY_LIMIT {
                queue.pop_front();
            }
        }

        for chunk in split_message(&answer) {
            if let Err(err) = msg.channel_id.say(&ctx.http, chunk).await {
                eprintln!("reply failed: {err}");
            }
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Command(command) = interaction else {
            return;
        };
        let content = match command.data.name.as_str() {
            "ping" => "Pong!".to_string(),
            "reset" => {
                HISTORY.lock().await.remove(&command.channel_id);
                "Dạ, em quên hết chuyện cũ rồi nè sensei yêu quý~ mình bắt đầu lại nhé!".to_string()
            }
            "help" => HELP.to_string(),
            _ => return,
        };
        let reply = CreateInteractionResponseMessage::new().content(content);
        let response = CreateInteractionResponse::Message(reply);
        if let Err(err) = command.create_response(&ctx.http, response).await {
            eprintln!("interaction reply failed: {err}");
        }
    }
}

#[tokio::main]
async fn main() {
    let cfg = Arc::new(Config::from_env());
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::DIRECT_MESSAGES;
    let handler = Handler { cfg: cfg.clone(), bot_id: OnceLock::new() };
    let mut client = match serenity::Client::builder(&cfg.token, intents)
        .event_handler(handler)
        .await
    {
        Ok(client) => client,
        Err(err) => {
            eprintln!("failed to build client: {err}");
            std::process::exit(1);
        }
    };
    if let Err(err) = client.start().await {
        eprintln!("client error: {err}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Body thật của ninerouter: JSON + sentinel `data: [DONE]` (đã cắt phần reasoning).
    const REAL: &str = "\n         \n{\"id\":\"gen-1\",\"choices\":[{\"index\":0,\"message\":{\"role\":\"assistant\",\"content\":\"Dạ em chào sensei yêu quý~\"},\"finish_reason\":\"stop\"}]}data: [DONE]\n\n";

    #[test]
    fn parses_body_with_trailing_done_sentinel() {
        assert_eq!(parse_content(REAL).unwrap(), "Dạ em chào sensei yêu quý~");
    }

    // Body SSE thật của ag/gemini-3.8-flash (đã cắt bớt), có cả chunk reasoning.
    const SSE: &str = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"Chào.\\n\\n\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Dạ em \"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"chào thầy yêu quý~\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );

    #[test]
    fn parses_sse_stream_and_drops_reasoning() {
        assert_eq!(parse_content(SSE).unwrap(), "Dạ em chào thầy yêu quý~");
    }

    #[test]
    fn surfaces_api_error_message() {
        let err = "{\"error\":{\"message\":\"[429] Rate limit exceeded: free-models-per-day\"}}";
        let msg = parse_content(err).unwrap_err();
        assert!(msg.contains("Rate limit exceeded"), "got: {msg}");
    }

    #[test]
    fn rejects_null_content() {
        let null = "{\"choices\":[{\"message\":{\"content\":null,\"reasoning\":\"...\"}}]}data: [DONE]";
        assert!(parse_content(null).is_err());
    }

    #[test]
    fn chunks_long_replies() {
        let chunks = split_message(&"a".repeat(4500));
        assert_eq!(chunks.iter().map(|c| c.chars().count()).collect::<Vec<_>>(), vec![2000, 2000, 500]);
    }

    #[test]
    fn strips_bot_mention() {
        let id = UserId::new(1570293369844932728);
        assert_eq!(strip_mentions(&format!("<@{id}> chào em"), id).trim(), "chào em");
    }

    #[test]
    fn builds_multimodal_user_message() {
        let text_only = ChatMessage::user("chào", &[]);
        assert!(text_only.content.is_string());

        let with_image = ChatMessage::user("ảnh nè", &["https://cdn.discordapp.com/a.png".into()]);
        let parts = with_image.content.as_array().expect("array content");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "https://cdn.discordapp.com/a.png");
    }
}
