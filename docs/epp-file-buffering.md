# EPP File Buffering Requirement

## Overview

The EPP (Endpoint Picker Processor) module requires NGINX to buffer all request bodies to temporary files instead of keeping them in memory. This is a **critical requirement** for memory safety.

## Why File Buffering is Required

EPP implements non-blocking async processing using a Tokio runtime. When request bodies are buffered in memory, NGINX's buffer pointers can become stale or invalid by the time EPP's async task tries to read them, causing crashes.

By forcing NGINX to always write request bodies to temporary files, EPP can safely read the body content from disk using file I/O (`pread()`), which is always safe regardless of timing.

## Required NGINX Configuration

Add this directive to your `http`, `server`, or `location` block:

```nginx
# Force all request bodies to be buffered to temporary files
# This is REQUIRED for EPP to work safely
client_body_buffer_size 1;
```

### Complete Example

```nginx
http {
    # Force file buffering for EPP safety
    client_body_buffer_size 1;
    
    # Also configure temp file location (optional)
    client_body_temp_path /tmp/nginx_client_body_temp 1 2;
    
    server {
        listen 8080;
        
        location / {
            # Enable EPP
            ngx_inference_epp on;
            ngx_inference_epp_endpoint "http://epp-service:9001";
            
            # Proxy to upstreams
            proxy_pass http://backend;
        }
    }
}
```

## What Happens Without File Buffering

If `client_body_buffer_size` is not set to `1`, NGINX will buffer small request bodies in memory. When EPP tries to read these memory buffers, you'll see:

1. **Warning logs**:
   ```
   [warn] EPP found no file-backed buffers - body may be in memory.
          Configure NGINX with 'client_body_buffer_size 1;' to force file buffering.
   ```

2. **Empty body processing**: EPP will receive an empty body and may fail to select the correct upstream.

3. **Potential crashes**: If memory pointers are accessed, worker processes may crash with:
   ```
   unsafe precondition(s) violated: slice::from_raw_parts requires the pointer to be aligned and non-null
   ```

## Performance Considerations

### Impact

- **Small requests (<1KB)**: ~10-20% latency increase due to disk I/O
- **Medium requests (1KB-1MB)**: Minimal impact (would be written to disk anyway)
- **Large requests (>1MB)**: No impact (always written to disk)

### Optimization

If performance is critical for small requests, you can:

1. **Use faster storage** for `client_body_temp_path` (e.g., tmpfs/RAM disk)
2. **Tune file system** settings (e.g., disable atime updates)
3. **Enable BBR** (Body-Based Routing) which already handles file I/O efficiently

Example using tmpfs:

```nginx
# Mount tmpfs (in /etc/fstab or systemd)
# tmpfs /tmp/nginx_body tmpfs size=1G,mode=1777 0 0

http {
    client_body_buffer_size 1;
    client_body_temp_path /tmp/nginx_body 1 2;
}
```

## Combined EPP + BBR Configuration

When using both EPP and BBR:

```nginx
http {
    # Required for EPP
    client_body_buffer_size 1;
    
    server {
        listen 8080;
        
        location / {
            # BBR extracts model from body (file-safe)
            ngx_inference_bbr on;
            ngx_inference_bbr_max_body_size 10485760;  # 10MB
            
            # EPP uses model and full body for routing (file-safe)
            ngx_inference_epp on;
            ngx_inference_epp_endpoint "http://epp-service:9001";
            
            proxy_pass http://$http_x_inference_upstream;
        }
    }
}
```

Both BBR and EPP are designed to work safely with file-buffered request bodies.

## Troubleshooting

### Check if File Buffering is Working

Look for these log messages at INFO level when EPP processes requests:

```
[info] EPP read 1234 bytes from file, total: 1234
```

If you see this warning instead, file buffering is NOT enabled:

```
[warn] EPP found no file-backed buffers - body may be in memory
```

### Verify Temp File Location

```bash
# Check NGINX error log for temp file creation
grep "client request body is buffered" /var/log/nginx/error.log

# Example output:
# [warn] a client request body is buffered to a temporary file /tmp/nginx_client_body_temp/0000000001
```

### Test Configuration

```bash
# Send a test request
curl -X POST http://localhost:8080/test \
  -H "Content-Type: application/json" \
  -d '{"model": "gpt-4", "messages": [{"role": "user", "content": "test"}]}'

# Check logs
tail -f /var/log/nginx/error.log | grep EPP
```

## Future Enhancements

In a future version, EPP may support direct memory buffer reading with additional safety checks. For now, file buffering is the recommended and **only supported** approach.

## Related Configuration

- `client_max_body_size`: Maximum request body size (default 1MB)
- `client_body_timeout`: Timeout for reading request body (default 60s)
- `client_body_in_file_only`: Alternative way to force file buffering (set to `on`)

## Summary

**Always configure `client_body_buffer_size 1;` when using EPP to ensure memory safety and prevent worker crashes.**
