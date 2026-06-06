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

fn extract_powershell_block(content: &str) -> Option<String> {
    let start_tag = "```powershell";
    let end_tag = "```";
    if let Some(start_idx) = content.find(start_tag) {
        let code_start = start_idx + start_tag.len();
        if let Some(end_idx) = content[code_start..].find(end_tag) {
            let command = content[code_start..(code_start + end_idx)].trim().to_string();
            return Some(command);
        }
    }
    None
}

fn execute_powershell_command(command: &str) -> Result<(bool, String)> {
    let output = std::process::Command::new("powershell")
        .arg("-NoProfile")
        .arg("-Command")
        .arg(command)
        .output()
        .context("Failed to execute PowerShell process")?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let mut combined = String::new();
    if !stdout.is_empty() {
        combined.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push_str("\n");
        }
        combined.push_str("Errors/Stderr:\n");
        combined.push_str(&stderr);
    }

    Ok((output.status.success(), combined))
}

fn load_memo() -> Option<String> {
    std::fs::read_to_string(".shokodex_memo").ok()
}

async fn listen_for_esc() -> Result<()> {
    use crossterm::event::{self, Event, KeyCode};
    use std::time::Duration;

    loop {
        if event::poll(Duration::from_millis(50)).context("Failed to poll terminal event")? {
            if let Event::Key(key_event) = event::read().context("Failed to read terminal event")? {
                if key_event.kind == event::KeyEventKind::Press && key_event.code == KeyCode::Esc {
                    return Ok(());
                }
            }
        }
        tokio::task::yield_now().await;
    }
}

async fn get_chat_response_interruptible(
    client: &Client,
    url: &str,
    request: &ChatCompletionRequest,
    interactive: bool,
) -> Result<Option<String>> {
    use crossterm::terminal;

    if interactive {
        terminal::enable_raw_mode().context("Failed to enable raw mode")?;
        print!("Thinking... (Press Esc to cancel)");
        io::stdout().flush().context("Failed to flush stdout")?;
    }

    struct RawModeGuard;
    impl Drop for RawModeGuard {
        fn drop(&mut self) {
            let _ = terminal::disable_raw_mode();
        }
    }
    let _guard = RawModeGuard;

    let api_future = get_chat_response(client, url, request);
    let esc_future = listen_for_esc();

    tokio::select! {
        res = api_future => {
            if interactive {
                let _ = terminal::disable_raw_mode();
                print!("\r");
                let _ = crossterm::execute!(io::stdout(), terminal::Clear(terminal::ClearType::CurrentLine));
                io::stdout().flush().context("Failed to flush stdout")?;
            }
            res.map(Some)
        }
        _ = esc_future => {
            if interactive {
                let _ = terminal::disable_raw_mode();
                println!("\n\x1b[31m[Thinking interrupted by user via Esc]\x1b[0m");
            }
            Ok(None)
        }
    }
}

async fn process_chat_cycle(
    client: &Client,
    url: &str,
    model: &str,
    system_prompt: &str,
    messages: &mut Vec<ChatMessage>,
    last_output: &mut String,
    interactive: bool,
) -> Result<()> {
    loop {
        // Load the latest memo and inject it dynamically into the system message context
        let current_memo = load_memo();
        let mut full_system_prompt = system_prompt.to_string();
        if let Some(memo) = current_memo {
            full_system_prompt.push_str("\n\n[MEMO / PROGRESS NOTES - read this to remember your current state]\n");
            full_system_prompt.push_str(&memo);
            full_system_prompt.push_str("\n[END OF MEMO]");
        }

        if let Some(first_msg) = messages.first_mut() {
            if first_msg.role == "system" {
                first_msg.content = full_system_prompt;
            }
        }

        let request_body = ChatCompletionRequest {
            model: model.to_string(),
            messages: messages.clone(),
            temperature: 0.2,
            max_tokens: 2048,
            stream: false,
        };

        let response_opt = get_chat_response_interruptible(client, url, &request_body, interactive).await?;

        let answer = match response_opt {
            Some(ans) => ans,
            None => {
                // Cancelled by Esc
                break;
            }
        };

        println!("\n• {answer}\n");
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: answer.clone(),
        });

        if let Some(cmd) = extract_powershell_block(&answer) {
            let confirm = match read_line_custom("\x1b[1;36mExecute PowerShell command? (y/N): \x1b[0m", last_output) {
                Ok(Some(inp)) => inp,
                Ok(None) => {
                    println!("Execution sequence interrupted by user.");
                    break;
                }
                Err(e) => {
                    eprintln!("Input error: {:#}", e);
                    break;
                }
            };
            let trimmed_confirm = confirm.trim().to_lowercase();

            if trimmed_confirm == "y" || trimmed_confirm == "yes" {
                println!("\x1b[33mExecuting command via PowerShell...\x1b[0m");
                match execute_powershell_command(&cmd) {
                    Ok((success, output)) => {
                        // Store output for Ctrl+O expansion
                        *last_output = output.clone();

                        if success {
                            println!("\n\x1b[32m[✔ PowerShell command executed successfully. Press Ctrl+O to expand output]\x1b[0m\n");
                        } else {
                            println!("\n\x1b[31m[✘ PowerShell command failed. Press Ctrl+O to expand output]\x1b[0m\n");
                        }

                        messages.push(ChatMessage {
                            role: "user".to_string(),
                            content: format!("Command output:\n{}", output),
                        });
                    }
                    Err(e) => {
                        println!("\x1b[31mExecution error: {:#}\x1b[0m", e);
                        messages.push(ChatMessage {
                            role: "user".to_string(),
                            content: format!("Command execution failed with internal error: {:#}", e),
                        });
                    }
                }
                // Continue loop to get the next LLM response with tool output (infinite loop allowed)
                continue;
            } else {
                println!("Execution cancelled by user.");
                break;
            }
        }
        break; // No command block found, cycle complete.
    }
    Ok(())
}

fn read_line_custom(prompt: &str, last_output: &str) -> Result<Option<String>> {
    use crossterm::{
        cursor,
        event::{self, Event, KeyCode, KeyModifiers},
        execute,
        terminal::{self, ClearType},
    };

    // Print the initial prompt
    print!("{}", prompt);
    io::stdout().flush().context("Failed to flush stdout")?;

    let mut buffer = String::new();
    let mut cursor_pos = 0; // index in character count

    terminal::enable_raw_mode().context("Failed to enable raw mode")?;

    struct RawModeGuard;
    impl Drop for RawModeGuard {
        fn drop(&mut self) {
            let _ = terminal::disable_raw_mode();
        }
    }
    let _guard = RawModeGuard;

    loop {
        if event::poll(std::time::Duration::from_millis(100)).context("Failed to poll terminal event")? {
            if let Event::Key(key_event) = event::read().context("Failed to read terminal event")? {
                if key_event.kind == event::KeyEventKind::Release {
                    continue; // Skip key release events on Windows
                }

                // Check for Esc
                if key_event.code == KeyCode::Esc {
                    terminal::disable_raw_mode().context("Failed to disable raw mode")?;
                    println!();
                    return Ok(None);
                }

                // Check for Ctrl + O (Expand output)
                if key_event.code == KeyCode::Char('o') && key_event.modifiers.contains(KeyModifiers::CONTROL) {
                    terminal::disable_raw_mode().context("Failed to disable raw mode")?;
                    
                    println!(); // Print newline to move off the prompt line
                    
                    if last_output.is_empty() {
                        println!("\x1b[38;5;244m[No command output available yet]\x1b[0m");
                    } else {
                        // Print output with light gray background and dark text
                        println!("\x1b[1;30;48;5;252m--- Expanded Command Output ---\x1b[0m");
                        for line in last_output.lines() {
                            println!("\x1b[38;5;235;48;5;252m{}\x1b[0m", line);
                        }
                        println!("\x1b[1;30;48;5;252m--------------------------------\x1b[0m");
                    }
                    
                    terminal::enable_raw_mode().context("Failed to re-enable raw mode")?;
                    
                    // Reprint prompt and buffer
                    print!("\r");
                    execute!(io::stdout(), terminal::Clear(ClearType::CurrentLine)).context("Failed to clear line")?;
                    print!("{}{}", prompt, buffer);
                    
                    // Reposition cursor
                    let offset = buffer.chars().count() - cursor_pos;
                    if offset > 0 {
                        execute!(io::stdout(), cursor::MoveLeft(offset as u16)).context("Failed to move cursor")?;
                    }
                    io::stdout().flush().context("Failed to flush stdout")?;
                    continue;
                }

                // Check for Ctrl + C / Ctrl + D (Exit)
                if (key_event.code == KeyCode::Char('c') && key_event.modifiers.contains(KeyModifiers::CONTROL))
                    || key_event.code == KeyCode::Char('d') && key_event.modifiers.contains(KeyModifiers::CONTROL)
                {
                    terminal::disable_raw_mode().context("Failed to disable raw mode")?;
                    println!("\nGoodbye!");
                    std::process::exit(0);
                }

                match key_event.code {
                    KeyCode::Enter => {
                        terminal::disable_raw_mode().context("Failed to disable raw mode")?;
                        println!();
                        break;
                    }
                    KeyCode::Backspace => {
                        if cursor_pos > 0 {
                            // Find the char index to remove
                            let char_idx = buffer.char_indices().nth(cursor_pos - 1).map(|(i, _)| i).unwrap_or(0);
                            buffer.remove(char_idx);
                            cursor_pos -= 1;
                            
                            print!("\r");
                            execute!(io::stdout(), terminal::Clear(ClearType::CurrentLine)).context("Failed to clear line")?;
                            print!("{}{}", prompt, buffer);
                            
                            let offset = buffer.chars().count() - cursor_pos;
                            if offset > 0 {
                                execute!(io::stdout(), cursor::MoveLeft(offset as u16)).context("Failed to move cursor")?;
                            }
                            io::stdout().flush().context("Failed to flush stdout")?;
                        }
                    }
                    KeyCode::Delete => {
                        if cursor_pos < buffer.chars().count() {
                            let char_idx = buffer.char_indices().nth(cursor_pos).map(|(i, _)| i).unwrap_or(0);
                            buffer.remove(char_idx);
                            
                            print!("\r");
                            execute!(io::stdout(), terminal::Clear(ClearType::CurrentLine)).context("Failed to clear line")?;
                            print!("{}{}", prompt, buffer);
                            
                            let offset = buffer.chars().count() - cursor_pos;
                            if offset > 0 {
                                execute!(io::stdout(), cursor::MoveLeft(offset as u16)).context("Failed to move cursor")?;
                            }
                            io::stdout().flush().context("Failed to flush stdout")?;
                        }
                    }
                    KeyCode::Left => {
                        if cursor_pos > 0 {
                            cursor_pos -= 1;
                            execute!(io::stdout(), cursor::MoveLeft(1)).context("Failed to move cursor left")?;
                            io::stdout().flush().context("Failed to flush stdout")?;
                        }
                    }
                    KeyCode::Right => {
                        if cursor_pos < buffer.chars().count() {
                            cursor_pos += 1;
                            execute!(io::stdout(), cursor::MoveRight(1)).context("Failed to move cursor right")?;
                            io::stdout().flush().context("Failed to flush stdout")?;
                        }
                    }
                    KeyCode::Home => {
                        if cursor_pos > 0 {
                            execute!(io::stdout(), cursor::MoveLeft(cursor_pos as u16)).context("Failed to move cursor to start")?;
                            cursor_pos = 0;
                            io::stdout().flush().context("Failed to flush stdout")?;
                        }
                    }
                    KeyCode::End => {
                        let total_chars = buffer.chars().count();
                        if cursor_pos < total_chars {
                            let diff = total_chars - cursor_pos;
                            execute!(io::stdout(), cursor::MoveRight(diff as u16)).context("Failed to move cursor to end")?;
                            cursor_pos = total_chars;
                            io::stdout().flush().context("Failed to flush stdout")?;
                        }
                    }
                    KeyCode::Char(c) => {
                        // Insert char at cursor_pos character index
                        let byte_idx = if cursor_pos == 0 {
                            0
                        } else {
                            buffer.char_indices().nth(cursor_pos).map(|(i, _)| i).unwrap_or(buffer.len())
                        };
                        buffer.insert(byte_idx, c);
                        cursor_pos += 1;
                        
                        print!("\r");
                        execute!(io::stdout(), terminal::Clear(ClearType::CurrentLine)).context("Failed to clear line")?;
                        print!("{}{}", prompt, buffer);
                        
                        let offset = buffer.chars().count() - cursor_pos;
                        if offset > 0 {
                            execute!(io::stdout(), cursor::MoveLeft(offset as u16)).context("Failed to move cursor")?;
                        }
                        io::stdout().flush().context("Failed to flush stdout")?;
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(Some(buffer))
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

    let system_prompt = "You are Shokodex, a helpful assistant with access to the local machine via PowerShell. \
                         If you need to query the system, run commands, list files, or perform operations to answer the user, \
                         provide the command in a code block formatted as:\n\
                         ```powershell\n\
                         <commands>\n\
                         ```\n\
                         The user will be prompted to approve the command before execution. \
                         Provide only one code block per turn. Keep explanations brief and precise.\n\
                         You have a persistent notes file '.shokodex_memo' in the current directory. \
                         You can read/write to it using PowerShell (Get-Content / Set-Content) to record memos of what you do \
                         so you do not lose track of your progress across multiple turns.";

    let mut last_output = String::new();

    if !user_prompt_args.trim().is_empty() {
        // Execute single query if CLI arguments are provided
        let mut messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: system_prompt.to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: user_prompt_args,
            },
        ];

        if let Err(e) = process_chat_cycle(&client, &url, &model, system_prompt, &mut messages, &mut last_output, false).await {
            eprintln!("\nExecution Error: {:#}\n", e);
        }
    } else {
        // Start interactive REPL loop with conversation history
        let mut messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: system_prompt.to_string(),
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
                    content: "(v0.0.1 - PowerShell enabled)".to_string(),
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
            let prompt = "> \x1b[93m";
            let input = match read_line_custom(prompt, &last_output) {
                Ok(Some(inp)) => inp,
                Ok(None) => {
                    println!("(Use 'exit' or 'quit' to exit)");
                    continue;
                }
                Err(e) => {
                    eprintln!("Input error: {:#}", e);
                    break;
                }
            };

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

            if let Err(e) = process_chat_cycle(&client, &url, &model, system_prompt, &mut messages, &mut last_output, true).await {
                eprintln!("\nError: {:#}\n", e);
                // Remove the user message on error so we don't corrupt history
                messages.pop();
            }
        }
    }

    Ok(())
}