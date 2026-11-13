mod prompt;
mod websearch;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;

use serenity::all::{
    ChannelId, Command, CommandOptionType, Context, CreateCommand, CreateCommandOption,
    CreateInteractionResponse, CreateInteractionResponseFollowup, CreateInteractionResponseMessage,
    EventHandler, GatewayIntents, Interaction, Message, Ready, UserId,
};
use tokio::sync::Mutex;

/// Fallback khi không đọc được `prompts/*.md`.
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
• `/search <từ khoá>` — em tra tin mới trên web rồi tóm tắt kèm nguồn
• `/help` — xem lại danh sách này
Ngoài ra thầy cứ nhắn riêng cho em, hoặc tag em trong server là em trả lời liền!
Em cũng đọc được ảnh thầy gửi, và tự tra web khi thầy hỏi tin tức / thời tiết / giá cả.";

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
    /// Instruction/skill nạp từ `prompts/*.md` (hoặc fallback nhúng).
    system: String,
    /// Gộp các tin nhắn đến trong cùng khoảng này thành 1 prompt.
    group_window: Duration,
    http: reqwest::Client,
}

/// ponytail: gộp tin theo kênh, chưa tách theo người gửi. Tách theo author khi cần.
#[derive(Default)]
struct Pending {
    generation: u64,
    lines: Vec<String>,
    images: Vec<String>,
}

static PENDING: LazyLock<Mutex<HashMap<ChannelId, Pending>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static GENERATION: AtomicU64 = AtomicU64::new(0);

impl Config {
    fn from_env() -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("failed to build HTTP client");
        let (system, source) = prompt::load(PERSONA);
        println!("system prompt: {} ({} ký tự)", source, system.chars().count());
        let group_window = std::env::var("DISCORD_GROUP_WINDOW_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(2000);
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
            system,
            group_window: Duration::from_millis(group_window),
            http,
        }
    }

    /// Gọi ninerouter (OpenAI-compatible). Retry 1 lần nếu lỗi transport.
    async fn chat(&self, history: &[ChatMessage]) -> Result<String, String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut messages = Vec::with_capacity(history.len() + 1);
        messages.push(ChatMessage::new("system", &self.system));
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

/// Biến nội dung tin nhắn thô thành thứ Reisa "đọc" được:
/// - `<:Ten:id>` / `<a:Ten:id>` -> `:Ten:` (để em ấy hiểu emoji, không phải bỏ qua)
/// - `<@id>` -> `@Tên`, mention chính bot thì bỏ
/// - `<@&id>` (role), `<#id>` (kênh) giữ nguyên dạng gọn
/// Trả kèm cờ `has_text`: tin chỉ có emoji/mention thì không có nội dung thật.
fn render_content(content: &str, bot_id: UserId, mentions: &[serenity::model::user::User]) -> (String, bool) {
    let mut out = String::with_capacity(content.len());
    let mut has_text = false;
    let mut rest = content;

    while let Some(start) = rest.find('<') {
        let (before, tail) = rest.split_at(start);
        out.push_str(before);
        has_text |= before.chars().any(|c| c.is_alphanumeric());

        let Some(end) = tail.find('>') else {
            out.push_str(tail);
            rest = "";
            break;
        };
        let token = &tail[..=end];
        let inner = &tail[1..end];

        if let Some(name) = emoji_name(inner) {
            out.push(':');
            out.push_str(name);
            out.push(':');
        } else if let Some(id) = inner.strip_prefix('@').map(|r| r.strip_prefix('!').unwrap_or(r)) {
            if id.parse::<u64>() == Ok(bot_id.get()) {
                // mention chính bot: bỏ hẳn, không tính là nội dung
            } else if let Some(user) = id
                .parse::<u64>()
                .ok()
                .and_then(|id| mentions.iter().find(|u| u.id.get() == id))
            {
                out.push('@');
                out.push_str(&user.name);
            } else {
                out.push_str("@ai_do");
            }
        } else {
            // role, channel, hoặc cú pháp khác: giữ nguyên
            out.push_str(token);
            has_text |= token.chars().any(|c| c.is_alphanumeric());
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    has_text |= rest.chars().any(|c| c.is_alphanumeric());

    (out.trim().to_string(), has_text)
}

/// `:name:id` (emoji thường) hoặc `a:name:id` (emoji động) -> `name`.
/// Đây là phần bên trong của `<...>`, nên luôn có dấu `:` trước tên.
fn emoji_name(inner: &str) -> Option<&str> {
    let inner = inner.strip_prefix('a').unwrap_or(inner);
    let inner = inner.strip_prefix(':')?;
    let (name, id) = inner.split_once(':')?;
    let name_ok = !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_');
    let id_ok = !id.is_empty() && id.chars().all(|c| c.is_ascii_digit());
    (name_ok && id_ok).then_some(name)
}

/// Lấy ảnh sensei gửi (bỏ GIF/sticker động — chỉ nhận ảnh tĩnh).
fn image_urls(msg: &Message) -> Vec<String> {
    msg.attachments
        .iter()
        .filter(|a| {
            a.content_type
                .as_deref()
                .is_some_and(|t| t.starts_with("image/") && t != "image/gif")
        })
        .take(4)
        .map(|a| a.url.clone())
        .collect()
}

/// Chờ hết cửa sổ gộp tin rồi lấy cả cụm; trả None nếu đã có tin mới hơn tiếp quản.
async fn take_ready(channel: ChannelId, generation: u64) -> Option<Pending> {
    let mut map = PENDING.lock().await;
    match map.get(&channel) {
        Some(pending) if pending.generation == generation => map.remove(&channel),
        _ => None,
    }
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
            CreateCommand::new("search")
                .description("Nhờ em tra thông tin mới trên web")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::String,
                        "tu_khoa",
                        "Thầy muốn em tìm gì ạ?",
                    )
                    .required(true),
                ),
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
        let (text, has_text) = render_content(&msg.content, bot_id, &msg.mentions);
        let images = image_urls(&msg);
        eprintln!(
            "msg ch={} author={} len={} images={} stickers={}",
            msg.channel_id,
            msg.author.name,
            msg.content.chars().count(),
            images.len(),
            msg.sticker_items.len()
        );

        // Lọc nhanh: sticker/GIF thuần, tin chỉ có emoji, tin rỗng.
        if msg.sticker_items.is_empty() == false && text.is_empty() && images.is_empty() {
            eprintln!("skip: sticker");
            return;
        }
        if text.is_empty() && images.is_empty() {
            eprintln!("skip: empty (mention-only, GIF, hoặc MESSAGE_CONTENT chưa cấp)");
            return;
        }
        if images.is_empty() && !has_text {
            eprintln!("skip: chỉ có emoji");
            return;
        }

        // Biết tên người nhắn (nick trong server) để Reisa gọi đúng người.
        let name = msg
            .member
            .as_ref()
            .and_then(|m| m.nick.clone())
            .or_else(|| msg.author.global_name.clone())
            .unwrap_or_else(|| msg.author.name.clone());
        let line = if text.is_empty() {
            format!("{name}: (vừa gửi ảnh, không kèm chữ)")
        } else {
            format!("{name}: {text}")
        };

        // Gộp các tin đến liên tiếp trong cửa sổ group_window thành 1 prompt.
        let generation = {
            let mut map = PENDING.lock().await;
            let pending = map.entry(msg.channel_id).or_default();
            pending.generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
            pending.lines.push(line);
            pending.images.extend(images);
            pending.images.truncate(4);
            pending.generation
        };
        let _ = msg.channel_id.broadcast_typing(&ctx.http).await;

        let ctx = ctx.clone();
        let cfg = self.cfg.clone();
        let channel = msg.channel_id;
        let window = self.cfg.group_window;
        tokio::spawn(async move {
            tokio::time::sleep(window).await;
            // Có tin mới hơn trong lúc chờ -> nhường lượt cho task đó.
            let Some(pending) = take_ready(channel, generation).await else {
                return;
            };
            let prompt = pending.lines.join("\n");
            eprintln!(
                "grouped {} tin -> {} ký tự, {} ảnh",
                pending.lines.len(),
                prompt.chars().count(),
                pending.images.len()
            );
            // Tự tra web khi sensei hỏi thứ cần dữ liệu mới.
            let prompt = if websearch::wants_search(&prompt) {
                let query = websearch::query_from_prompt(&prompt);
                match websearch::search(&cfg.http, &query).await {
                    Ok(hits) => {
                        eprintln!("search \"{query}\": {} kết quả", hits.len());
                        format!("{}\n\n{prompt}", websearch::format_hits(&query, &hits))
                    }
                    Err(err) => {
                        eprintln!("search error: {err}");
                        prompt
                    }
                }
            } else {
                prompt
            };
            let _ = channel.broadcast_typing(&ctx.http).await;

            let history = {
                let mut map = HISTORY.lock().await;
                let queue = map.entry(channel).or_default();
                queue.push_back(ChatMessage::user(&prompt, &pending.images));
                while queue.len() > HISTORY_LIMIT {
                    queue.pop_front();
                }
                queue.iter().cloned().collect::<Vec<_>>()
            };

            let started = std::time::Instant::now();
            let answer = match cfg.chat(&history).await {
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
                let queue = map.entry(channel).or_default();
                queue.push_back(ChatMessage::new("assistant", answer.clone()));
                while queue.len() > HISTORY_LIMIT {
                    queue.pop_front();
                }
            }

            for chunk in split_message(&answer) {
                if let Err(err) = channel.say(&ctx.http, chunk).await {
                    eprintln!("reply failed: {err}");
                }
            }
        });
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Command(command) = interaction else {
            return;
        };
        // /search cần defer vì tra web + gọi AI lâu hơn hạn 3s của Discord.
        if command.data.name == "search" {
            let query = command
                .data
                .options
                .first()
                .and_then(|o| o.value.as_str())
                .unwrap_or_default()
                .to_string();
            let http = ctx.http.clone();
            let cfg = self.cfg.clone();
            let deferred = CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new());
            if let Err(err) = command.create_response(&ctx.http, deferred).await {
                eprintln!("defer failed: {err}");
                return;
            }
            tokio::spawn(async move {
                let answer = match websearch::search(&cfg.http, &query).await {
                    Ok(hits) => {
                        let context = websearch::format_hits(&query, &hits);
                        let messages = vec![
                            ChatMessage::new("system", &cfg.system),
                            ChatMessage::user(&context, &[]),
                        ];
                        match cfg.chat(&messages).await {
                            Ok(text) => text,
                            Err(err) => {
                                eprintln!("search chat error: {err}");
                                // Vẫn trả kết quả thô để sensei không mất thông tin.
                                context
                            }
                        }
                    }
                    Err(err) => {
                        eprintln!("search error: {err}");
                        "Huhu em không tra được gì hết sensei yêu quý ơi, thầy thử lại sau nhé!".to_string()
                    }
                };
                let followup = CreateInteractionResponseFollowup::new().content(answer);
                if let Err(err) = command.create_followup(&http, followup).await {
                    eprintln!("search followup failed: {err}");
                }
            });
            return;
        }

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
    fn strips_bot_mention_and_reads_emoji() {
        let id = UserId::new(1570293369844932728);
        let (text, has) = render_content(&format!("<@{id}> chào em"), id, &[]);
        assert_eq!(text, "chào em");
        assert!(has);

        // Custom emoji: đọc được tên, và tin chỉ có emoji thì không có nội dung thật.
        let (text, has) = render_content("<:Seia:1530583004869558392>", id, &[]);
        assert_eq!(text, ":Seia:");
        assert!(!has, "emoji-only phải bị coi là không có nội dung");
    }

    #[test]
    fn reads_animated_emoji_and_rejects_fake_ones() {
        assert_eq!(emoji_name("a:Seia:1530583004869558392"), Some("Seia"));
        assert_eq!(emoji_name(":Seia:1530583004869558392"), Some("Seia"));
        assert_eq!(emoji_name("chữ thường"), None);
        assert_eq!(emoji_name(":Seia:khong-phai-id"), None);
    }

    #[test]
    fn resolves_known_mention_to_name() {
        let bot = UserId::new(1);
        let mut other = serenity::model::user::User::default();
        other.id = UserId::new(42);
        other.name = "vinh".into();
        let (text, has) = render_content("<@42> ơi", bot, std::slice::from_ref(&other));
        assert_eq!(text, "@vinh ơi");
        assert!(has);
    }

    #[tokio::test]
    async fn groups_messages_until_window_closes() {
        let channel = ChannelId::new(999);
        {
            let mut map = PENDING.lock().await;
            let pending = map.entry(channel).or_default();
            pending.generation = 7;
            pending.lines.push("a: tin 1".into());
            pending.lines.push("a: tin 2".into());
        }
        // Task cũ (generation lệch) không được lấy cụm.
        assert!(take_ready(channel, 6).await.is_none());
        let taken = take_ready(channel, 7).await.expect("generation khớp thì lấy được");
        assert_eq!(taken.lines, vec!["a: tin 1", "a: tin 2"]);
        // Lấy rồi thì cụm biến mất.
        assert!(take_ready(channel, 7).await.is_none());
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
