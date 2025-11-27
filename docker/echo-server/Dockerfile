FROM node:20-alpine
WORKDIR /app
RUN npm init -y && npm install express
COPY custom-echo-server.js .
EXPOSE 80
CMD ["node", "custom-echo-server.js"]