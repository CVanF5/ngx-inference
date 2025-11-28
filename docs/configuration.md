# Configuration Reference

This document provides a complete reference for all ngx-inference module directives and configuration options.

## Module Directives

### BBR (Body-Based Routing) Directives

#### `inference_bbr`

- **Syntax**: `inference_bbr on|off`
- **Default**: `off`
- **Context**: `http`, `server`, `location`

Enables or disables Body-Based Routing functionality. When enabled, the module will parse JSON request bodies to extract model information.

```nginx
location /v1/chat/completions {
    inference_bbr on;
    # ... other configuration
}
```

#### `inference_bbr_max_body_size`

- **Syntax**: `inference_bbr_max_body_size <bytes>`
- **Default**: `10485760` (10MB)
- **Context**: `http`, `server`, `location`

Sets the maximum request body size (in bytes) that BBR will process. Requests larger than this size will be rejected with HTTP 413 (if fail-closed) or skipped (if fail-open).

```nginx
inference_bbr_max_body_size 52428800; # 50MB
```

#### `inference_bbr_header_name`

- **Syntax**: `inference_bbr_header_name <name>`
- **Default**: `X-Gateway-Model-Name`
- **Context**: `http`, `server`, `location`

Specifies the header name where BBR will store the extracted model information.

```nginx
inference_bbr_header_name X-Model-ID;
```

#### `inference_bbr_failure_mode_allow`

- **Syntax**: `inference_bbr_failure_mode_allow on|off`
- **Default**: `off`
- **Context**: `http`, `server`, `location`

Controls the failure mode for BBR processing:
- `off` (fail-closed): Return HTTP 502 on BBR errors
- `on` (fail-open): Continue processing on BBR errors

```nginx
inference_bbr_failure_mode_allow on; # Fail-open for development
```

### EPP (Endpoint Picker Processor) Directives

#### `inference_epp`

- **Syntax**: `inference_epp on|off`
- **Default**: `off`
- **Context**: `http`, `server`, `location`

Enables or disables Endpoint Picker Processor functionality. When enabled, the module will communicate with an external gRPC service for intelligent upstream selection.

```nginx
location /v1/chat/completions {
    inference_epp on;
    inference_epp_endpoint "epp-service:9001";
    # ... other configuration
}
```

#### `inference_epp_endpoint`

- **Syntax**: `inference_epp_endpoint <address>`
- **Default**: none (required if EPP is enabled)
- **Context**: `http`, `server`, `location`

Specifies the gRPC endpoint address for the external processor service.

```nginx
inference_epp_endpoint "localhost:9001";
inference_epp_endpoint "epp-service.default.svc.cluster.local:9001";
```

#### `inference_epp_timeout_ms`

- **Syntax**: `inference_epp_timeout_ms <milliseconds>`
- **Default**: `200`
- **Context**: `http`, `server`, `location`

Sets the timeout for EPP gRPC calls in milliseconds.

```nginx
inference_epp_timeout_ms 5000; # 5 second timeout
```

#### `inference_epp_header_name`

- **Syntax**: `inference_epp_header_name <name>`
- **Default**: `X-Inference-Upstream`
- **Context**: `http`, `server`, `location`

Specifies the header name where EPP will store the selected upstream endpoint information.

```nginx
inference_epp_header_name X-Selected-Upstream;
```

#### `inference_epp_failure_mode_allow`

- **Syntax**: `inference_epp_failure_mode_allow on|off`
- **Default**: `off`
- **Context**: `http`, `server`, `location`

Controls the failure mode for EPP processing:
- `off` (fail-closed): Return HTTP 502 on EPP errors
- `on` (fail-open): Continue processing on EPP errors

```nginx
inference_epp_failure_mode_allow off; # Fail-closed for production
```

## NGINX Variables

### `$inference_upstream`

Contains the upstream endpoint selected by the EPP processor. This variable can be used in `proxy_pass` directives and other NGINX contexts.

```nginx
location /api/ {
    inference_epp on;
    inference_epp_endpoint "epp-service:9001";

    # Use the dynamically selected upstream
    proxy_pass http://$inference_upstream;
}
```

## Configuration Examples

### Basic BBR Configuration

```nginx
server {
    listen 80;

    location /v1/chat/completions {
        inference_bbr on;
        inference_bbr_max_body_size 20971520; # 20MB

        proxy_pass http://ai-backend:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    }
}
```

### Combined BBR + EPP Configuration

```nginx
server {
    listen 80;

    location /v1/chat/completions {
        # Enable BBR for model extraction
        inference_bbr on;
        inference_bbr_max_body_size 104857600; # 100MB
        inference_bbr_failure_mode_allow off; # Fail-closed

        # Enable EPP for intelligent routing
        inference_epp on;
        inference_epp_endpoint "epp-service:9001";
        inference_epp_timeout_ms 3000;
        inference_epp_failure_mode_allow off; # Fail-closed

        # Route to dynamically selected upstream
        proxy_pass http://$inference_upstream;
        proxy_http_version 1.1;
        proxy_set_header Connection "";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;

        # Timeouts for AI workloads
        proxy_connect_timeout 30s;
        proxy_send_timeout 300s;
        proxy_read_timeout 300s;
    }
}
```

### Development vs Production Settings

#### Development Configuration

```nginx
# Development: fail-open, detailed logging
inference_bbr on;
inference_bbr_failure_mode_allow on;  # Continue on errors
inference_epp on;
inference_epp_failure_mode_allow on;  # Continue on errors
inference_epp_timeout_ms 10000;       # Longer timeout

error_log /var/log/nginx/error.log debug;
```

#### Production Configuration

```nginx
# Production: fail-closed, optimized timeouts
inference_bbr on;
inference_bbr_failure_mode_allow off; # Fail on errors
inference_epp on;
inference_epp_failure_mode_allow off; # Fail on errors
inference_epp_timeout_ms 3000;        # Shorter timeout

error_log /var/log/nginx/error.log warn;
```

## Best Practices

### Performance

1. **Body Size Limits**: Set appropriate `inference_bbr_max_body_size` based on your AI model requirements
2. **Timeouts**: Configure `inference_epp_timeout_ms` to balance responsiveness and reliability
3. **Connection Pooling**: Use `keepalive` directives in upstream blocks for better performance

### Security

1. **Fail-Closed**: Use fail-closed mode (`*_failure_mode_allow off`) in production
2. **Body Size**: Limit request body sizes to prevent DoS attacks
3. **Timeouts**: Set reasonable timeouts to prevent resource exhaustion

### Monitoring

1. **Logging**: Enable appropriate log levels for debugging and monitoring
2. **Metrics**: Monitor success/failure rates of BBR and EPP processing
3. **Health Checks**: Implement health checks for external processor services

## Troubleshooting

### Common Issues

1. **Module not loading**: Check module path and NGINX configuration syntax
2. **BBR not extracting models**: Verify JSON request body format and content-type headers
3. **EPP connection failures**: Check external processor service availability and network connectivity
4. **High memory usage**: Adjust `inference_bbr_max_body_size` and implement proper body size limits