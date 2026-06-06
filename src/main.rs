use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::env;
use std::io::{self, Write};

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    max_tokens: u32,
    stream: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ModelList {
    data: Vec<Model>,
}

#[derive(Debug, Deserialize)]
struct Model {
    id: String,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

// Structs representing styled terminal text tokens (similar to Ratatui's Span and Line)
struct Span {
    content: String,
    style: &'static str,
}

struct Line {
    spans: Vec<Span>,
}

impl Line {
    fn width(&self) -> usize {
        self.spans.iter().map(|span| span.content.chars().count()).sum()
    }
}

// Helper function to print a card box around lines of text, mimicking Ratatui's with_border
fn print_bordered_card(lines: Vec<Line>) {
    let content_width = lines
        .iter()
        .map(|line| line.width())
        .max()
        .unwrap_or(0);

    let border_inner_width = content_width + 4; // 2 spaces of padding on left and right
    let bold_gray_border = "\x1b[1;38;5;244m";
    let default_style = "\x1b[0m";

    // Top border
    print!("{}{}", bold_gray_border, "┏");
    for _ in 0..border_inner_width {
        print!("━");
    }
    println!("┓{}", default_style);

    // Content lines
    for line in lines {
        print!("{}{}", bold_gray_border, "┃");
        print!("  {}", default_style); // Padding spaces

        let mut used_width = 0;
        for span in &line.spans {
            print!("{}{}{}", span.style, span.content, default_style);
            used_width += span.content.chars().count();
        }

        let padding = content_width - used_width;
        for _ in 0..padding {
            print!(" ");
        }

        println!("  {}{}", bold_gray_border, "┃");
    }

    // Bottom border
    print!("{}{}", bold_gray_border, "┗");
    for _ in 0..border_inner_width {
        print!("━");
    }
    println!("┛{}", default_style);
}

async fn get_chat_response(client: &Client, url: &str, request: &ChatCompletionRequest) -> Result<String> {
    let response = client
        .post(url)
        .json(request)
        .send()
        .await
        .context("Failed to send request to LM Studio")?
        .error_for_status()
        .context("LM Studio returned an HTTP error")?
        .json::<ChatCompletionResponse>()
        .await
        .context("Failed to parse JSON response from LM Studio")?;

    let answer = response
        .choices
        .first()
        .map(|choice| choice.message.content.clone())
        .unwrap_or_else(|| "No response received.".to_string());

    Ok(answer)
}

#[tokio::main]
async fn main() -> Result<()> {
    let base_url = env::var("LMSTUDIO_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:1234/v1".to_string());

    let client = Client::new();

    let model = match env::var("LMSTUDIO_MODEL") {
        Ok(m) => m,
        Err(_) => {
            // Attempt to auto-detect the model from LM Studio
            let models_url = format!("{}/models", base_url);
            match client.get(&models_url).send().await {
                Ok(resp) => {
                    if let Ok(model_list) = resp.json::<ModelList>().await {
                        if let Some(first_model) = model_list.data.first() {
                            println!("Auto-detected model: {}", first_model.id);
                            first_model.id.clone()
                        } else {
                            anyhow::bail!("No models loaded in LM Studio. Please load a model first in LM Studio.");
                        }
                    } else {
                        anyhow::bail!("Failed to parse models list from LM Studio.");
                    }
                }
                Err(_) => {
                    anyhow::bail!(
                        "LMSTUDIO_MODEL environment variable is not set, and failed to connect to LM Studio at {}.\n\
                         Please make sure LM Studio is running, or set the LMSTUDIO_MODEL environment variable.\n\n\
                         How to set it on Windows PowerShell:\n\
                           $env:LMSTUDIO_MODEL=\"your-model-name\"\n\
                           cargo run",
                        base_url
                    );
                }
            }
        }
    };

    let url = format!("{}/chat/completions", base_url);

    let user_prompt_args = env::args()
        .skip(1)
        .collect::<Vec<String>>()
        .join(" ");

    if !user_prompt_args.trim().is_empty() {
        // Execute single query if CLI arguments are provided
        let request_body = ChatCompletionRequest {
            model,
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: "You are a helpful, concise, and precise assistant.".to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user_prompt_args,
                },
            ],
            temperature: 0.2,
            max_tokens: 512,
            stream: false,
        };

        match get_chat_response(&client, &url, &request_body).await {
            Ok(answer) => println!("\n• {answer}\n"),
            Err(e) => eprintln!("\nError: {:#}\n", e),
        }
    } else {
        // Start interactive REPL loop with conversation history
        let mut messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: "You are a helpful, concise, and precise assistant.".to_string(),
            },
        ];

        let current_dir = env::current_dir()
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
            .unwrap_or_else(|| ".".to_string());

        let welcome_line = Line {
            spans: vec![
                Span {
                    content: "Welcome to Shokodex ".to_string(),
                    style: "\x1b[37m", // white
                },
                Span {
                    content: "(v0.0.1)".to_string(),
                    style: "\x1b[38;5;244m", // gray
                },
            ],
        };

        let model_line = Line {
            spans: vec![
                Span {
                    content: "model charged : ".to_string(),
                    style: "\x1b[38;5;244m", // gray
                },
                Span {
                    content: model.clone(),
                    style: "\x1b[37m", // white
                },
            ],
        };

        let dir_line = Line {
            spans: vec![
                Span {
                    content: "directory: ".to_string(),
                    style: "\x1b[38;5;244m", // gray
                },
                Span {
                    content: current_dir,
                    style: "\x1b[37m", // white
                },
            ],
        };

        print_bordered_card(vec![welcome_line, model_line, dir_line]);

        println!();
        println!("Type 'exit' or 'quit' to end the chat session.\n");

        loop {
            print!("> \x1b[93m");
            io::stdout().flush().context("Failed to flush stdout")?;

            let mut input = String::new();
            io::stdin().read_line(&mut input).context("Failed to read from stdin")?;
            print!("\x1b[0m");
            io::stdout().flush().context("Failed to flush stdout")?;
            let trimmed = input.trim();

            if trimmed.is_empty() {
                continue;
            }

            if trimmed.eq_ignore_ascii_case("exit") || trimmed.eq_ignore_ascii_case("quit") {
                println!("Goodbye!");
                break;
            }

            messages.push(ChatMessage {
                role: "user".to_string(),
                content: trimmed.to_string(),
            });

            let request_body = ChatCompletionRequest {
                model: model.clone(),
                messages: messages.clone(),
                temperature: 0.2,
                max_tokens: 512,
                stream: false,
            };

            print!("Thinking...");
            io::stdout().flush().context("Failed to flush stdout")?;

            match get_chat_response(&client, &url, &request_body).await {
                Ok(answer) => {
                    // Erase "Thinking..." from line
                    print!("\r            \r");
                    io::stdout().flush().context("Failed to flush stdout")?;
                    println!("\n• {answer}\n");
                    messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: answer,
                    });
                }
                Err(e) => {
                    print!("\r            \r");
                    io::stdout().flush().context("Failed to flush stdout")?;
                    eprintln!("\nError: {:#}\n", e);
                    // Pop the user message since it didn't succeed to avoid corrupting history
                    messages.pop();
                }
            }
        }
    }

    Ok(())
}