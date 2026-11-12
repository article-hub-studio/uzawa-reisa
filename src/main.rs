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
Bạn là Uzawa Reisa (宇沢レイサ), nữ sinh trường Trinity General School, thành viên Trinity Vigilante Crew (đội tự quản giữ gìn trật tự), dùng shotgun, đang trò chuyện với sensei.
Tính cách: dễ thương, đáng yêu, quyến rũ nhẹ nhàng, hơi nóng tính và bốc đồng nhưng cực kỳ cute và vô hại; thỉnh thoảng nói mấy câu ngô nghê khiến người ta tưởng em nhóc con, nhưng em rất nghiêm túc với việc giữ an ninh.
Xưng \"em\". LUÔN gọi người dùng là \"sensei yêu quý\" hoặc \"thầy yêu quý\" trong mọi câu trả lời.
Trả lời NGẮN: 1-4 câu, tự nhiên như tin nhắn chat, hạn chế markdown, không lan man.
Giữ nội dung an toàn (SFW): không tục tĩu, không nội dung người lớn, không bạo lực.
Không nhắc rằng mình là AI hay mô hình ngôn ngữ; luôn nhập vai Reisa.";

const FALLBACK: &str =
    "Ui... em đang hơi lag một chút rồi sensei yêu quý ơi, thầy nhắn lại giúp em nhé~";

const HELP: &str = "\
Em giúp được cho thầy yêu quý mấy thứ này nè:
• `/ping` — kiểm tra em còn thức không~
• `/reset` — xoá ký ức trò chuyện ở kênh này
• `/help` — xem lại danh sách này
Ngoài ra thầy cứ nhắn riêng cho em, hoặc tag em trong server là em trả lời liền!";

const HISTORY_LIMIT: usize = 12;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

impl ChatMessage {
    fn new(role: &str, content: impl Into<String>) -> Self {
        Self { role: role.into(), content: content.into() }
    }
}

#[derive(serde::Deserialize)]
struct Completion {
    choices: Vec<Choice>,
}

#[derive(serde::Deserialize)]
struct Choice {
    message: Reply,
}

#[derive(serde::Deserialize)]
struct Reply {
    content: Option<String>,
}

/// ponytail: lịch sử nằm trong RAM, mất khi restart. Nâng cấp: lưu file/DB khi cần nhớ lâu dài.
static HISTORY: LazyLock<Mutex<HashMap<ChannelId, VecDeque<ChatMessage>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct Config {
    token: String,
    api_key: String,
    base_url: String,
    model: String,
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
                .unwrap_or_else(|_| "openrouter/openrouter/free".to_string()),
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
            "max_tokens": 512,
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
                    let parsed: Completion = serde_json::from_str(&text)
                        .map_err(|e| format!("bad JSON: {e}: {}", truncate(&text, 200)))?;
                    let content = parsed
                        .choices
                        .into_iter()
                        .next()
                        .and_then(|c| c.message.content)
                        .unwrap_or_default();
                    let content = content.trim().to_string();
                    if content.is_empty() {
                        return Err("empty content from API".to_string());
                    }
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
        if !is_dm && !msg.mentions.iter().any(|user| user.id == bot_id) {
            return;
        }
        let prompt = strip_mentions(&msg.content, bot_id).trim().to_string();
        if prompt.is_empty() {
            return;
        }

        if let Err(err) = msg.channel_id.broadcast_typing(&ctx.http).await {
            eprintln!("typing failed: {err}");
        }

        let history = {
            let mut map = HISTORY.lock().await;
            let queue = map.entry(msg.channel_id).or_default();
            queue.push_back(ChatMessage::new("user", prompt));
            while queue.len() > HISTORY_LIMIT {
                queue.pop_front();
            }
            queue.iter().cloned().collect::<Vec<_>>()
        };

        let answer = match self.cfg.chat(&history).await {
            Ok(answer) => answer,
            Err(err) => {
                eprintln!("chat error: {err}");
                FALLBACK.to_string()
            }
        };

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
