import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { dirname, extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const root = normalize(join(dirname(fileURLToPath(import.meta.url)), "../.."));
const assetsRoot = join(root, "apps/dashboard/assets");
const port = Number(process.env.PLAYWRIGHT_MOCK_PORT || 4183);

const contentTypes = {
  ".css": "text/css; charset=utf-8",
  ".html": "text/html; charset=utf-8",
  ".js": "application/javascript; charset=utf-8",
};

async function readAsset(pathname) {
  const asset = pathname === "/" ? "index.html" : pathname.replace(/^\/+/, "");
  const fullPath = join(assetsRoot, asset);
  if (!fullPath.startsWith(assetsRoot)) {
    return null;
  }
  try {
    return {
      body: await readFile(fullPath),
      contentType: contentTypes[extname(fullPath)] || "application/octet-stream",
    };
  } catch {
    return null;
  }
}

createServer(async (req, res) => {
  const url = new URL(req.url || "/", `http://${req.headers.host || "127.0.0.1"}`);
  const asset = await readAsset(url.pathname) || await readAsset("/index.html");
  if (!asset) {
    res.writeHead(404, { "Content-Type": "text/plain; charset=utf-8" });
    res.end("not found");
    return;
  }
  res.writeHead(200, { "Content-Type": asset.contentType });
  res.end(asset.body);
}).listen(port, "127.0.0.1", () => {
  console.log(`dashboard mock server listening on http://127.0.0.1:${port}`);
});
