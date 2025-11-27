# Examples Directory

This directory contains practical examples for using the ngx-inference module in different scenarios.

## Basic Configuration (`basic-config/`)

A simple NGINX configuration that demonstrates:
- Loading the ngx-inference module
- Enabling Body-Based Routing (BBR) for OpenAI-compatible APIs
- Basic proxy configuration for AI backends

**Use case**: Simple AI gateway with model extraction from JSON request bodies.

## Advanced Configuration (`advanced-config/`)

A comprehensive production-ready configuration that includes:
- Both BBR and EPP (Endpoint Picker Processor) features
- SSL/TLS termination
- Multiple upstream pools for different AI models
- Enhanced logging with inference variables
- Security headers and monitoring endpoints
- Optimized settings for large AI workloads