# Leech-RS API Docs

Base URL:

```text
http://127.0.0.1:8000
```

The server host and port are configured by `config.toml` under `[server]`.

## Overview

Leech-RS is a local compatibility proxy for use.ai headless WebSocket completions. It exposes:

- OpenAI-compatible chat completions at `/v1/chat/completions`
- Anthropic-compatible messages at `/v1/messages`
- Model listing at `/v1/models`
- Health, account pool, config, and proxy status endpoints
- Image helper endpoints
- Basic OpenAI and Anthropic tool-call protocol compatibility for coding agents

## Status Endpoints

### `GET /health`

Returns proxy health and current warm account count.

Example:

```bash
curl -s http://127.0.0.1:8000/health
```

Response:

```json
{
  "status": "ok",
  "fresh_accounts": 100,
  "send_success_rate": 1.0,
  "reasons": ["all systems nominal"]
}
```

### `GET /bank`

Returns account pool status.

```bash
curl -s http://127.0.0.1:8000/bank
```

Response:

```json
{
  "mode": "headless-ws",
  "warm_accounts": 100,
  "pool_target": 100,
  "status": "ok"
}
```

### `GET /config`

Returns the loaded runtime config snapshot.

```bash
curl -s http://127.0.0.1:8000/config
```

Response includes:

- `server_host`
- `server_port`
- `pool_size`
- `signup_delay_ms`
- `account_ttl_sec`
- `proxy_tor`
- `tor_socks`
- `tor_ports`
- `tor_instances`

### `POST /config`

Partially updates `config.toml`.

Example:

```bash
curl -s -X POST http://127.0.0.1:8000/config \
  -H "Content-Type: application/json" \
  -d '{
    "direct": {
      "ws_idle_timeout_sec": 120,
      "direct_ws_retries": 3
    },
    "models": {
      "default": "gpt-5-4"
    }
  }'
```

Accepted top-level sections:

- `server`
- `direct`
- `account_pool`
- `proxy`
- `models`
- `thinking`

Some updates are saved immediately but require restart to affect already-initialized components:

- `server`
- `account_pool`
- `proxy`

Direct request settings are reloaded by request paths and can affect future calls.

### `GET /proxies`

Returns dynamic Tor proxy state and load metrics.

```bash
curl -s http://127.0.0.1:8000/proxies
```

Response:

```json
{
  "proxies": ["socks5h://127.0.0.1:9050"],
  "proxy_count": 1,
  "load": {
    "requests_per_second": 0.5,
    "window_requests": 2
  }
}
```

## Models

### `GET /v1/models`

Lists supported model aliases.

```bash
curl -s http://127.0.0.1:8000/v1/models
```

Response is OpenAI-style:

```json
{
  "object": "list",
  "data": [
    {
      "id": "gpt-5-4",
      "object": "model",
      "owned_by": "leech",
      "label": "OpenAI GPT-5.4"
    }
  ]
}
```

Unknown model aliases generally fall back through model resolution rather than hard-failing.

## OpenAI Chat Completions

### `POST /v1/chat/completions`

OpenAI-compatible chat completions endpoint.

Supported request fields:

- `model`
- `messages`
- `stream`
- `thinking`
- `tools`
- `tool_choice`

### Non-Streaming Text

```bash
curl -s -X POST http://127.0.0.1:8000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-5-4",
    "messages": [
      {"role": "user", "content": "What is the capital of France?"}
    ]
  }'
```

Response:

```json
{
  "id": "chatcmpl-...",
  "object": "chat.completion",
  "created": 1782759206,
  "model": "gpt-5-4",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "Paris."
      },
      "finish_reason": "stop"
    }
  ]
}
```

### Streaming

```bash
curl -N -X POST http://127.0.0.1:8000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-5-4",
    "messages": [
      {"role": "user", "content": "Count from 1 to 5"}
    ],
    "stream": true
  }'
```

Streaming emits OpenAI-style SSE events:

```text
data: {"id":"chatcmpl-...","object":"chat.completion.chunk","choices":[{"delta":{"role":"assistant"},"finish_reason":null,"index":0}]}

data: {"id":"chatcmpl-...","object":"chat.completion.chunk","choices":[{"delta":{"content":"1, 2, 3"},"finish_reason":null,"index":0}]}

data: {"id":"chatcmpl-...","object":"chat.completion.chunk","choices":[{"delta":{},"finish_reason":"stop","index":0}]}

data: [DONE]
```

### Thinking

`thinking` may be:

- `true`
- `"low"`, `"medium"`, `"high"`, or `"max"`
- an object such as `{"type_":"enabled","budget_tokens":5000}`

Example:

```json
{
  "model": "claude-opus-4-8",
  "messages": [{"role": "user", "content": "Explain quantum computing in one sentence"}],
  "thinking": true
}
```

Non-streaming responses include a top-level `thinking` field when thinking tags are parsed.

### Images and Files

The proxy supports structured `content` values.

String content:

```json
{"role": "user", "content": "Hello"}
```

Object image content:

```json
{
  "role": "user",
  "content": {
    "image": "data:image/png;base64,...",
    "filename": "image.png",
    "text": "What is this image?"
  }
}
```

Object file URL content:

```json
{
  "role": "user",
  "content": {
    "file_url": "https://example.com/file.pdf",
    "filename": "file.pdf",
    "text": "Summarize this file"
  }
}
```

OpenAI-style image array:

```json
{
  "role": "user",
  "content": [
    {"type": "text", "text": "What is this image?"},
    {"type": "image_url", "image_url": {"url": "https://httpbin.org/image/png"}}
  ]
}
```

OpenAI-style file array:

```json
{
  "role": "user",
  "content": [
    {"type": "text", "text": "Analyze this file"},
    {"type": "file", "file": {"url": "https://example.com/file.pdf", "filename": "file.pdf"}}
  ]
}
```

Base64 and data URI files are uploaded to `files.use.ai` before the WebSocket frame is sent. Remote image URLs are downloaded and re-uploaded to `files.use.ai`. Non-image file URLs are passed through as file URLs.

### OpenAI Tool Calls

The proxy accepts `tools` and `tool_choice`.

Example:

```json
{
  "model": "gpt-5-4",
  "messages": [
    {"role": "user", "content": "Read package.json"}
  ],
  "tools": [
    {
      "type": "function",
      "function": {
        "name": "read_file",
        "description": "Read a file from the workspace",
        "parameters": {
          "type": "object",
          "properties": {
            "path": {"type": "string"}
          },
          "required": ["path"]
        }
      }
    }
  ]
}
```

Non-streaming tool-call response:

```json
{
  "choices": [
    {
      "message": {
        "role": "assistant",
        "content": null,
        "tool_calls": [
          {
            "id": "call_...",
            "type": "function",
            "function": {
              "name": "read_file",
              "arguments": "{\"path\":\"package.json\"}"
            }
          }
        ]
      },
      "finish_reason": "tool_calls"
    }
  ]
}
```

Tool result follow-up messages are accepted:

```json
{
  "role": "tool",
  "tool_call_id": "call_...",
  "content": "file contents here"
}
```

The proxy converts tool calls by prompting the upstream model to emit:

```text
<tool_use>
{"name":"tool_name","input":{"key":"value"}}
</tool_use>
```

This is compatibility glue. Tool execution is performed by the client, such as Codex, Cursor, Continue, Cline, Roo, or another OpenAI-compatible agent.

## Anthropic Messages

### `POST /v1/messages`

Anthropic-compatible messages endpoint.

Supported request fields:

- `model`
- `messages`
- `system`
- `stream`
- `max_tokens`
- `thinking`
- `tools`
- `tool_choice`

### Non-Streaming Text

```bash
curl -s -X POST http://127.0.0.1:8000/v1/messages \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-opus-4-8",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

Response:

```json
{
  "id": "msg_...",
  "type": "message",
  "role": "assistant",
  "content": [
    {
      "type": "text",
      "text": "Hi there!"
    }
  ],
  "model": "claude-opus-4-8",
  "stop_reason": "end_turn",
  "stop_sequence": null,
  "usage": {
    "input_tokens": 0,
    "output_tokens": 10
  }
}
```

### Streaming

```bash
curl -N -X POST http://127.0.0.1:8000/v1/messages \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-opus-4-8",
    "messages": [{"role": "user", "content": "Count from 1 to 5"}],
    "stream": true
  }'
```

Anthropic-style SSE events:

```text
data: {"type":"message_start","message":{...}}

data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"1, 2, 3, 4, 5"}}

data: {"type":"content_block_stop","index":0}

data: {"type":"message_stop"}

data: [DONE]
```

### Anthropic Thinking

When `thinking: true`, the proxy asks the upstream model to produce:

```text
<thinking>...</thinking>
<response>...</response>
```

Non-streaming responses include:

```json
{
  "thinking": "reasoning text",
  "content": [{"type": "text", "text": "final answer"}]
}
```

Streaming responses emit:

- `thinking_delta`
- then normal `content_block_delta` text deltas

### Anthropic Images

Anthropic base64 image content is supported:

```json
{
  "role": "user",
  "content": [
    {"type": "text", "text": "What is this image?"},
    {
      "type": "image",
      "source": {
        "type": "base64",
        "media_type": "image/png",
        "data": "..."
      }
    }
  ]
}
```

### Anthropic Tool Calls

The proxy accepts Anthropic `tools` and `tool_choice`.

Example:

```json
{
  "model": "claude-opus-4-8",
  "messages": [
    {"role": "user", "content": "Read package.json"}
  ],
  "tools": [
    {
      "name": "read_file",
      "description": "Read a file from the workspace",
      "input_schema": {
        "type": "object",
        "properties": {
          "path": {"type": "string"}
        },
        "required": ["path"]
      }
    }
  ]
}
```

Non-streaming tool response:

```json
{
  "type": "message",
  "role": "assistant",
  "content": [
    {
      "type": "tool_use",
      "id": "toolu_...",
      "name": "read_file",
      "input": {"path": "package.json"}
    }
  ],
  "stop_reason": "tool_use"
}
```

Tool result follow-up:

```json
{
  "role": "user",
  "content": [
    {
      "type": "tool_result",
      "tool_use_id": "toolu_...",
      "content": "file contents here"
    }
  ]
}
```

Streaming tool responses emit:

- `message_start`
- `ping`
- `content_block_start` with `type: "tool_use"`
- `content_block_delta` with `input_json_delta`
- `content_block_stop`
- `message_delta` with `stop_reason: "tool_use"`
- `message_stop`
- `[DONE]`

This is intended for Claude Code, Claude Code GUI, and other Anthropic-compatible coding agents. Tool execution remains client-side.

## Image Helper Endpoints

### `POST /v1/chat/with-image`

Accepts JSON with base64/data URI image input.

```bash
curl -s -X POST http://127.0.0.1:8000/v1/chat/with-image \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-5-4",
    "image": "data:image/png;base64,...",
    "filename": "image.png",
    "question": "What is this image?"
  }'
```

Response:

```json
{
  "model": "gpt-5-4",
  "choices": [
    {
      "message": {
        "role": "assistant",
        "content": "description"
      }
    }
  ]
}
```

### `POST /v1/chat/upload-image`

Accepts multipart upload.

Fields:

- `file`
- `question`
- `model`

Example:

```bash
curl -s -X POST http://127.0.0.1:8000/v1/chat/upload-image \
  -F "file=@test.png" \
  -F "question=What is this image?" \
  -F "model=gpt-5-4"
```

Response:

```json
{
  "model": "gpt-5-4",
  "analysis": "description"
}
```

## Error Responses

Errors are JSON objects:

```json
{
  "error": "message"
}
```

Common errors:

- concurrency limit errors
- account acquisition failures
- WebSocket timeout or remote close
- invalid base64
- failed image download
- failed upload to `files.use.ai`
- empty upstream reply

## Operational Notes

### Account Pool

The proxy maintains a warm account pool. `/health` and `/bank` show current warm account counts. If warm accounts are low immediately after startup, wait for the pool to refill.

### Tor Proxies

Configured Tor ports are registered at startup. The scale controller can add/remove Tor proxies based on load.

### Streaming Latency

The proxy emits valid SSE framing, but the first content token depends on upstream WebSocket latency. Tool-call streaming buffers until a whole tool call can be parsed, because partial JSON tool calls can break strict clients.

### Config Updates

`POST /config` saves `config.toml`. Some settings require restart:

- server host and port
- account pool size and refill behavior
- proxy topology

### Windows PowerShell Test Command

If using Git Bash from PowerShell:

```powershell
& 'C:\Program Files\Git\bin\bash.exe' test.sh
```

## Compatibility Targets

OpenAI-compatible:

- Codex
- Cursor
- Continue.dev
- Cline / Roo Code when configured for OpenAI-compatible APIs
- Zed custom OpenAI-compatible provider
- Aider-style clients

Anthropic-compatible:

- Claude Code
- Claude Code GUI
- Cline / Roo Code when configured for Anthropic-compatible APIs

The proxy implements basic tool protocol compatibility. It does not execute tools itself.
