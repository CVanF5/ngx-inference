#!/bin/bash

# Test script to validate hybrid memory/file BBR support
# Tests small (memory), medium (file), large (9MB), and very large (12MB) payloads

cd "$(dirname "$0")/.."

# Create an isolated temp directory for all generated payloads
TMP_DIR=$(mktemp -d)
# Ensure cleanup on exit, regardless of success or failure
cleanup() {
  [[ -n "$TMP_DIR" && -d "$TMP_DIR" ]] && rm -rf "$TMP_DIR"
}
trap cleanup EXIT

echo "Testing small body (memory buffered)..."
curl -s http://localhost:8081/bbr-test \
  -H 'Content-Type: application/json' \
  --data '{"model":"gpt-4","prompt":"small request"}' | \
  jq '.request.headers."x-gateway-model-name"'

echo ""
echo "Testing large body (file buffered)..."

# Create a large JSON payload (>16KB to exceed default client_body_buffer_size)
# Use temporary file to avoid command line argument limits
temp_file=$(mktemp "$TMP_DIR/large-XXXXXX.json")
{
  echo -n '{"model":"claude-3-opus","prompt":"'
  printf "data-%04d " {1..2000}  # ~14KB of content
  echo '","max_tokens":1000}'
} > "$temp_file"

curl -s http://localhost:8081/bbr-test \
  -H 'Content-Type: application/json' \
  --data-binary "@$temp_file" | \
  jq '.request.headers."x-gateway-model-name"'

rm "$temp_file"

echo ""
echo "Testing large body within BBR limits (10MB)..."

# Create a large payload that approaches the 10MB BBR limit
temp_file=$(mktemp "$TMP_DIR/large-9mb-XXXXXX.json")
{
  echo -n '{"model":"claude-3-sonnet","prompt":"'
  # Create ~9MB of content to stay safely under 10MB limit
  printf "data-%08d " {1..540000}  # ~9MB of content
  echo '","max_tokens":4000}'
} > "$temp_file"

# Capture both response and HTTP status
response=$(curl -s http://localhost:8081/bbr-test \
  -H 'Content-Type: application/json' \
  --data-binary "@$temp_file" \
  -w "HTTPSTATUS:%{http_code}")

http_status=$(echo "$response" | grep -o 'HTTPSTATUS:[0-9]*' | cut -d: -f2)
response_body=$(echo "$response" | sed 's/HTTPSTATUS:[0-9]*$//')

rm "$temp_file"

if [[ "$http_status" != "200" ]]; then
  echo "⚠ 9MB payload rejected - HTTP $http_status"
  echo "  → Response: $(echo "$response_body" | head -n 1 | sed 's/<[^>]*>//g' | xargs)"
else
  echo "✓ 9MB payload accepted (HTTP $http_status): $(echo "$response_body" | jq -r '.request.headers."x-gateway-model-name" // "No model name found"')"
fi

echo ""
echo "Testing very large body (exceeds BBR 10MB limit)..."

# Create a payload that exceeds the BBR 10MB limit  
temp_file=$(mktemp "$TMP_DIR/large-20mb-XXXXXX.json")
{
  echo -n '{"model":"claude-3-haiku","prompt":"'
  # Create ~20MB of content to definitely exceed 10MB BBR limit
  printf "data-%08d " {1..1200000}  # ~20MB of content
  echo '","max_tokens":4000}'
} > "$temp_file"

payload_size=$(wc -c < "$temp_file")
echo "  Payload size: $payload_size bytes (~$((payload_size / 1024 / 1024))MB)"

# Capture both response and HTTP status
response=$(curl -s http://localhost:8081/bbr-test \
  -H 'Content-Type: application/json' \
  --data-binary "@$temp_file" \
  -w "HTTPSTATUS:%{http_code}")

http_status=$(echo "$response" | grep -o 'HTTPSTATUS:[0-9]*' | cut -d: -f2)
response_body=$(echo "$response" | sed 's/HTTPSTATUS:[0-9]*$//')

rm "$temp_file"

if [[ "$http_status" == "413" ]]; then
  echo "✓ Very large payload correctly rejected by BBR module (HTTP $http_status)"
  echo "  → Error: $(echo "$response_body" | head -n 1 | sed 's/<[^>]*>//g' | xargs)"
elif [[ "$http_status" == "502" ]]; then
  echo "✓ Very large payload rejected by system limits (HTTP $http_status)"
  echo "  → Error: $(echo "$response_body" | head -n 1 | sed 's/<[^>]*>//g' | xargs)"
elif [[ "$http_status" == "200" ]]; then
  echo "⚠ Unexpected: very large payload was accepted (HTTP $http_status)"
  echo "  → Model extracted: $(echo "$response_body" | jq -r '.request.headers."x-gateway-model-name" // "No model"')"
else
  echo "⚠ Unexpected HTTP status: $http_status"
  echo "  → Response: $(echo "$response_body" | head -n 1 | sed 's/<[^>]*>//g' | xargs)"
fi

echo ""
echo "Test completed!"