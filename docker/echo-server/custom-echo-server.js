const express = require('express');
const app = express();
const port = process.env.PORT || 80;

// Configure Express to accept large bodies
app.use(express.json({ limit: '50mb' }));
app.use(express.urlencoded({ limit: '50mb', extended: true }));

// Echo endpoint that returns request details
app.use('/', (req, res) => {
  const response = {
    host: {
      hostname: req.hostname,
      ip: req.ip,
      ips: req.ips
    },
    http: {
      method: req.method,
      baseUrl: req.baseUrl,
      originalUrl: req.originalUrl,
      protocol: req.protocol
    },
    request: {
      params: req.params,
      query: req.query,
      cookies: req.cookies,
      body: req.body,
      headers: req.headers
    },
    environment: process.env
  };

  const bodySize = req.body ? JSON.stringify(req.body).length : 0;
  console.log(`${new Date().toISOString()} ${req.method} ${req.originalUrl} - Body size: ${bodySize} bytes`);

  res.json(response);
});

app.listen(port, '0.0.0.0', () => {
  console.log(`Custom echo server listening on port ${port} with 50MB body limit`);
});