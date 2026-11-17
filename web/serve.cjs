// 零依赖静态服务器（本目录自包含）
//   node serve.cjs                → http://127.0.0.1:8080（被占用则自动往后找 8081..8099）
//   PORT=9000 node serve.cjs      → 指定端口
// 需要 http 才能用 AudioWorklet 与 fetch wasm（file:// 不行）
const http = require("http");
const fs = require("fs");
const path = require("path");

const root = __dirname;
const startPort = Number(process.env.PORT || 8080);
const MAX_TRIES = 20;

const types = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".cjs": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".wasm": "application/wasm",
  ".bin": "application/octet-stream",
  ".css": "text/css; charset=utf-8",
  ".md": "text/markdown; charset=utf-8",
};

const server = http.createServer((req, res) => {
  const url = decodeURIComponent((req.url || "/").split("?")[0]);
  const file = path.join(root, url === "/" ? "index.html" : url);
  if (!file.startsWith(root)) {
    res.writeHead(403, { "Content-Type": "text/plain; charset=utf-8" }).end("403 forbidden");
    return;
  }
  fs.readFile(file, (err, data) => {
    if (err) {
      res.writeHead(404, { "Content-Type": "text/plain; charset=utf-8" }).end("404 " + url);
      return;
    }
    res.writeHead(200, {
      "Content-Type": types[path.extname(file)] || "application/octet-stream",
      "Cache-Control": "no-store",
    });
    res.end(data);
  });
});

let port = startPort;
let tries = 0;

function listen() {
  server.listen(port, "127.0.0.1");
}

server.on("listening", () => {
  console.log("");
  console.log("  DX7 (WASM) 播放器已启动：");
  console.log(`  →  http://127.0.0.1:${port}/`);
  if (port !== startPort) console.log(`  （${startPort} 被占用，已自动改用 ${port}）`);
  console.log("  Ctrl+C 退出");
  console.log("");
});

server.on("error", (err) => {
  if (err.code === "EADDRINUSE" && tries < MAX_TRIES) {
    tries++;
    port++;
    listen();
    return;
  }
  if (err.code === "EADDRINUSE") {
    console.error(`端口 ${startPort}..${port} 都被占用。请换一个：`);
    console.error("  PORT=9000 node serve.cjs");
  } else {
    console.error("启动失败：", err.message);
  }
  process.exit(1);
});

process.on("SIGINT", () => {
  server.close(() => process.exit(0));
});

listen();
