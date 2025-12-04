# Millama

AI-powered Telegram message assistant that helps you craft intelligent responses using LLM models.

## Features

- 🤖 AI-powered message drafts using any OpenAI-compatible API (Groq, OpenAI, Ollama, etc.)
- 📝 Configurable per-user system prompts
- ✅ Inline button approval workflow
- 📊 Structured logging with tracing
- ⚙️ Configuration via `config.toml`
- 🔒 Session management with SQLite
- 🧪 Comprehensive CI/CD with GitHub Actions

## Installation

### Prerequisites

- Rust 1.75 or later
- A Telegram account
- API key for your chosen LLM provider (Groq, OpenAI, or local Ollama)

### Building

```bash
cargo build --release
```

## Configuration

1. Copy the example configuration:

```bash
cp config.toml.example config.toml
```

2. Edit `config.toml` with your settings:

```toml
[telegram]
api_id = 12345678
api_hash = "your_api_hash_here"

[ai]
# Works with any OpenAI-compatible API
api_key = "your_api_key_here"
api_url = "https://api.groq.com/openai/v1/chat/completions"  # or OpenAI, Ollama, etc.
model = "meta-llama/llama-4-maverick-17b-128e-instruct"
temperature = 1.5

[settings]
session_file = "userbot.session"
debounce_seconds = 1
history_limit = 25

[[users]]
id = 123456789
name = "John Doe"
system_prompt = "Be professional and concise"
```

### Getting Your Telegram Credentials

1. Go to https://my.telegram.org/apps
2. Create a new application
3. Copy your `api_id` and `api_hash`

### Getting User IDs

Use [@userinfobot](https://t.me/userinfobot) on Telegram to get user IDs.

## Usage

Run the bot:

```bash
cargo run --release
```

Or with custom config:

```bash
cargo run --release -- --config /path/to/config.toml
```

### CLI Options

```
Usage: millama [OPTIONS]

Options:
  -c, --config <CONFIG>  Path to configuration file [default: config.toml]
  -d, --debug            Enable debug logging
  -t, --trace            Enable trace logging
  -h, --help             Print help
```

### Logging

Control logging with `RUST_LOG` environment variable:

```bash
RUST_LOG=millama=debug cargo run
```

Or use the CLI flags:

```bash
cargo run -- --debug   # Debug level
cargo run -- --trace   # Trace level (very verbose)
```

## How It Works

1. The bot monitors messages from configured tracked users
2. After a configurable debounce period (default 1 second), it fetches message history
3. The history is sent to your configured AI provider (Groq, OpenAI, Ollama, etc.) with the user's system prompt
4. An AI-generated draft is sent to you for approval
5. Approve the message to send it or reject it

## Development

### Running Tests

```bash
cargo test
```

### Formatting

```bash
cargo fmt
```

### Linting

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

### Before Committing

Run all checks:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```

## CI/CD

The project includes comprehensive GitHub Actions workflows:

- ✅ Tests on every push and PR
- 🎨 Format checking with rustfmt
- 📎 Linting with clippy
- 🌙 Nightly Rust checks (continue-on-error)
- 🏗️ Release builds

## Architecture

```
src/
├── main.rs      - Main application entry point
├── config.rs    - Configuration parsing and validation
├── llm.rs       - OpenAI-compatible API client
└── lib.rs       - Library and tests
```

## Configuration Reference

### `[telegram]`
- `api_id` (required): Your Telegram API ID
- `api_hash` (required): Your Telegram API hash
- `bot_token` (optional): Bot token for alternative approval methods

### `[ai]`
- `api_key` (required): Your API key (may be optional for local Ollama)
- `api_url` (required): OpenAI-compatible API endpoint
  - Groq: `https://api.groq.com/openai/v1/chat/completions`
  - OpenAI: `https://api.openai.com/v1/chat/completions`
  - Local Ollama: `http://localhost:11434/v1/chat/completions`
- `model` (required): Model to use
  - Groq: `meta-llama/llama-4-maverick-17b-128e-instruct`
  - OpenAI: `gpt-4`, `gpt-3.5-turbo`, etc.
  - Ollama: `llama2`, `mistral`, etc.
- `temperature` (optional): Generation temperature 0.0-2.0 (default: 1.5)

### `[settings]`
- `session_file` (optional): Session file path (default: userbot.session)
- `debounce_seconds` (optional): Delay before generating draft (default: 1)
- `history_limit` (optional): Max messages in history (default: 25)

### `[[users]]`
- `id` (required): Telegram user ID
- `name` (required): Display name for logs
- `system_prompt` (required): AI system prompt for this user

## Security

- Never commit your `config.toml` file (it's in `.gitignore`)
- Keep your API keys secure
- Session files contain authentication data - protect them

## License

This project is provided as-is for educational and personal use.

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Run tests and linting
5. Submit a pull request

## Troubleshooting

### "Failed to read config file"
- Make sure `config.toml` exists
- Check file permissions

### "Failed to parse config file"
- Validate your TOML syntax
- Ensure all required fields are present

### "Update error"
- Check your internet connection
- Verify Telegram credentials are correct

### "API Error"
- Verify your API key is valid
- Check that your `api_url` is correct
- Check API rate limits
- For Ollama, ensure the server is running (`ollama serve`)

## Roadmap

- [x] Support for multiple LLM providers (OpenAI-compatible APIs)
- [ ] Web interface for configuration
- [ ] Message templates
- [ ] Conversation branching
- [ ] Multi-language support
