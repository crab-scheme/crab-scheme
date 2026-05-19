// Vanilla Node.js HTTP server. No framework — Express would
// only add overhead. Matches the "node-raw" tier on TFB.
const http = require("http");

const body = "Hello, World!";
const server = http.createServer((req, res) => {
  if (req.url === "/plain") {
    res.writeHead(200, {
      "Content-Type": "text/plain",
      "Content-Length": body.length,
    });
    res.end(body);
  } else {
    res.writeHead(404);
    res.end();
  }
});

const port = parseInt(process.argv[2] || "0", 10);
server.listen(port, "127.0.0.1", () => {
  const a = server.address();
  console.log(`node on ${a.address}:${a.port}`);
});
