#!/usr/bin/env bash
set -u

BASE_URL="${BASE_URL:-http://127.0.0.1:8000}"
TEST_IMAGE="${TEST_IMAGE:-iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==}"
TEST_IMAGE_URL="${TEST_IMAGE_URL:-https://httpbin.org/image/png}"

HAS_JQ=0
if command -v jq >/dev/null 2>&1; then
  HAS_JQ=1
fi

PYTHON_BIN=""
if command -v python >/dev/null 2>&1; then
  PYTHON_BIN="python"
elif command -v python3 >/dev/null 2>&1; then
  PYTHON_BIN="python3"
fi

section() {
  printf '\n=== %s ===\n' "$1"
}

post_json() {
  local path="$1"
  local body="$2"
  curl -sS -X POST "$BASE_URL$path" \
    -H "Content-Type: application/json" \
    -d "$body"
}

stream_json() {
  local path="$1"
  local body="$2"
  curl -N -X POST "$BASE_URL$path" \
    -H "Content-Type: application/json" \
    -d "$body"
}

pretty_json() {
  if [[ "$HAS_JQ" == "1" ]]; then
    jq .
  elif [[ -n "$PYTHON_BIN" ]]; then
    "$PYTHON_BIN" -m json.tool
  else
    cat
  fi
}

filter_json() {
  local filter="$1"
  if [[ "$HAS_JQ" == "1" ]]; then
    jq "$filter"
  elif [[ -n "$PYTHON_BIN" ]]; then
    "$PYTHON_BIN" -c '
import json
import sys

data = json.load(sys.stdin)
expr = sys.argv[1]

def dig(value, path):
    cur = value
    for part in path.split("."):
        if not part:
            continue
        while "[" in part:
            name, rest = part.split("[", 1)
            if name:
                cur = cur.get(name) if isinstance(cur, dict) else None
            index, part = rest.split("]", 1)
            cur = cur[int(index)] if isinstance(cur, list) and index.isdigit() and int(index) < len(cur) else None
            if cur is None:
                return None
        if part:
            cur = cur.get(part) if isinstance(cur, dict) else None
        if cur is None:
            return None
    return cur

if expr == ".":
    out = data
elif expr == ".data | length":
    out = len(data.get("data", [])) if isinstance(data, dict) else None
elif expr.startswith("."):
    choices = [part.strip() for part in expr.split("//")]
    out = None
    for choice in choices:
        if choice.startswith("."):
            out = dig(data, choice[1:])
            if out is not None:
                break
else:
    out = data

print(json.dumps(out, ensure_ascii=True))
' "$filter"
  else
    cat
  fi
}

section "Health"
curl -sS "$BASE_URL/health" | pretty_json

section "Bank"
curl -sS "$BASE_URL/bank" | pretty_json

section "Config"
curl -sS "$BASE_URL/config" | pretty_json

section "Proxies"
curl -sS "$BASE_URL/proxies" | pretty_json

section "Models"
curl -sS "$BASE_URL/v1/models" | filter_json '.data | length'

section "OpenAI Text"
post_json "/v1/chat/completions" '{
  "model": "gpt-5-4",
  "messages": [{"role": "user", "content": "What is the capital of France?"}]
}' | filter_json '.choices[0].message.content // .error'

section "OpenAI Thinking"
post_json "/v1/chat/completions" '{
  "model": "claude-opus-4-8",
  "messages": [{"role": "user", "content": "Explain quantum computing in one sentence"}],
  "thinking": true
}' | filter_json '{thinking, content: .choices[0].message.content, error}'

section "OpenAI Streaming"
stream_json "/v1/chat/completions" '{
  "model": "gpt-5-4",
  "messages": [{"role": "user", "content": "Count from 1 to 5"}],
  "stream": true
}' | head -20

section "OpenAI Streaming With Thinking"
stream_json "/v1/chat/completions" '{
  "model": "claude-opus-4-8",
  "messages": [{"role": "user", "content": "Explain quantum computing in one sentence"}],
  "stream": true,
  "thinking": true
}' | grep -E "thinking|content" | head -10

section "Image Base64 Object"
post_json "/v1/chat/completions" "{
  \"model\": \"gpt-5-4\",
  \"messages\": [{
    \"role\": \"user\",
    \"content\": {
      \"image\": \"data:image/png;base64,$TEST_IMAGE\",
      \"text\": \"What do you see in this image?\"
    }
  }]
}" | filter_json '.choices[0].message.content // .error'

section "Image URL Object"
post_json "/v1/chat/completions" '{
  "model": "gpt-5-4",
  "messages": [{
    "role": "user",
    "content": {
      "image": "'"$TEST_IMAGE_URL"'",
      "text": "What is this image?"
    }
  }]
}' | filter_json '.choices[0].message.content // .error'

section "Image OpenAI Array"
post_json "/v1/chat/completions" '{
  "model": "gpt-5-4",
  "messages": [{
    "role": "user",
    "content": [
      {"type": "text", "text": "What is this image?"},
      {"type": "image_url", "image_url": {"url": "'"$TEST_IMAGE_URL"'"}}
    ]
  }]
}' | filter_json '.choices[0].message.content // .error'

section "Image Endpoint Base64"
post_json "/v1/chat/with-image" "{
  \"image\": \"data:image/png;base64,$TEST_IMAGE\",
  \"question\": \"What color is this pixel?\"
}" | filter_json '.choices[0].message.content // .error'

section "Anthropic Text"
post_json "/v1/messages" '{
  "model": "claude-opus-4-8",
  "messages": [{"role": "user", "content": "Hello"}]
}' | filter_json '.content[0].text // .error'

section "Anthropic System"
post_json "/v1/messages" '{
  "model": "claude-opus-4-8",
  "system": "You are a helpful assistant.",
  "messages": [{"role": "user", "content": "What is the capital of France?"}]
}' | filter_json '.content[0].text // .error'

section "Anthropic Thinking"
post_json "/v1/messages" '{
  "model": "claude-opus-4-8",
  "messages": [{"role": "user", "content": "Explain quantum computing in one sentence"}],
  "thinking": true
}' | filter_json '{thinking, response: .content[0].text, error}'

section "Anthropic Image Base64"
post_json "/v1/messages" "{
  \"model\": \"claude-opus-4-8\",
  \"messages\": [{
    \"role\": \"user\",
    \"content\": [
      {\"type\": \"text\", \"text\": \"What is this image?\"},
      {\"type\": \"image\", \"source\": {\"type\": \"base64\", \"media_type\": \"image/png\", \"data\": \"$TEST_IMAGE\"}}
    ]
  }]
}" | filter_json '.content[0].text // .error'

section "Anthropic Streaming"
stream_json "/v1/messages" '{
  "model": "claude-opus-4-8",
  "messages": [{"role": "user", "content": "Count from 1 to 5"}],
  "stream": true
}' | head -20

section "Anthropic Streaming With Thinking"
stream_json "/v1/messages" '{
  "model": "claude-opus-4-8",
  "messages": [{"role": "user", "content": "Explain quantum computing in one sentence"}],
  "stream": true,
  "thinking": true
}' | grep -E "thinking_delta|text_delta" | head -10

section "File URL Object"
post_json "/v1/chat/completions" '{
  "model": "gpt-5-4",
  "messages": [{
    "role": "user",
    "content": {
      "file_url": "https://www.w3.org/WAI/ER/tests/xhtml/testfiles/resources/pdf/dummy.pdf",
      "filename": "dummy.pdf",
      "text": "What is this document about?"
    }
  }]
}' | filter_json '.choices[0].message.content // .error'

section "File OpenAI Array"
post_json "/v1/chat/completions" '{
  "model": "gpt-5-4",
  "messages": [{
    "role": "user",
    "content": [
      {"type": "text", "text": "Analyze this file"},
      {"type": "file", "file": {"url": "https://www.w3.org/WAI/ER/tests/xhtml/testfiles/resources/pdf/dummy.pdf", "filename": "dummy.pdf"}}
    ]
  }]
}' | filter_json '.choices[0].message.content // .error'

section "Invalid Model"
post_json "/v1/chat/completions" '{
  "model": "non-existent-model",
  "messages": [{"role": "user", "content": "Hello"}]
}' | filter_json '.error // .choices[0].message.content'

section "Empty Messages"
post_json "/v1/chat/completions" '{"model": "gpt-5-4", "messages": []}' |
  filter_json '.error // .choices[0].message.content'

section "Invalid Image Base64"
post_json "/v1/chat/completions" '{
  "model": "gpt-5-4",
  "messages": [{
    "role": "user",
    "content": {
      "image": "not-valid-base64!!!",
      "text": "What is this?"
    }
  }]
}' | filter_json '.error // .'

if [[ "${RUN_MULTIPART:-0}" == "1" ]]; then
  section "Multipart Image Upload"
  printf '%s' "$TEST_IMAGE" | base64 -d > test.png
  curl -sS -X POST "$BASE_URL/v1/chat/upload-image" \
    -F "file=@test.png" \
    -F "question=What is this image?" \
    -F "model=gpt-5-4" | filter_json '.analysis // .error // .'
  rm -f test.png
fi

if [[ "${RUN_CONCURRENCY:-0}" == "1" ]]; then
  section "Concurrency"
  for _ in {1..10}; do
    post_json "/v1/chat/completions" '{"model":"gpt-5-mini","messages":[{"role":"user","content":"Say hi"}]}' &
  done
  wait
fi

if [[ "${RUN_LOAD:-0}" == "1" ]]; then
  section "Proxy Scaling Load"
  curl -sS "$BASE_URL/proxies" | filter_json '.proxy_count'
  for _ in {1..50}; do
    post_json "/v1/chat/completions" '{"model":"gpt-5-mini","messages":[{"role":"user","content":"Hi"}]}' >/dev/null &
  done
  wait
  sleep 10
  curl -sS "$BASE_URL/proxies" | filter_json '{proxy_count, load}'
fi

section "Summary"
echo "Proxy status:"
curl -sS "$BASE_URL/health" | filter_json '{status, fresh_accounts}'
echo "Proxy count:"
curl -sS "$BASE_URL/proxies" | filter_json '{proxy_count, load}'
echo "Model count:"
curl -sS "$BASE_URL/v1/models" | filter_json '.data | length'
