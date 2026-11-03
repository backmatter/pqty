#!/usr/bin/env node
// Functional browser-cache fixture: no production service, account, or corpus.
// Usage: node scripts/check-browser-files.mjs [playwright-module] [chromium-executable]
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";
import { dirname } from "node:path";

const repo = resolve(import.meta.dirname, "..");
const { chromium } = await import(process.argv[2] ? pathToFileURL(resolve(process.argv[2])).href : "playwright");
const executablePath = process.argv[3];
const root = mkdtempSync(join(process.env.TMPDIR ?? tmpdir(), "pqty-browser-files-"));
const profile = join(root, "profile");
const store = join(root, "store");
const publicFiles = join(root, "public");
mkdirSync(publicFiles);
const distribution = join(root, "distribution");
const tlpdb = join(distribution, "tlpkg/texlive.tlpdb");
const style = "tex/latex/browser-demo/browser-demo.sty";
const extra = "tex/latex/browser-demo/unopened.def";
const text = "% requested package file\n";
for (const [path, bytes] of [[style, text], [extra, "% never requested\n"]]) {
  const file = join(distribution, "texmf-dist", path);
  mkdirSync(resolve(file, ".."), { recursive: true });
  writeFileSync(file, bytes);
}
mkdirSync(resolve(tlpdb, ".."), { recursive: true });
writeFileSync(tlpdb, `name 00texlive.config\ncategory TLCore\ndepend release/2026\n\nname browser-demo\ncategory Package\nrevision 1\nrunfiles size=1\n texmf-dist/${style}\n texmf-dist/${extra}\n`);
const pqty = join(repo, "target/debug", process.platform === "win32" ? "pqty.exe" : "pqty");
const run = args => execFileSync(pqty, ["--no-config", "--offline", ...args], { cwd: root, encoding: "utf8", timeout: 30_000 });
const stats = { preparations: 0, fileRequests: 0, fileBytes: 0, exports: 0 };
let manifest;
const manifestPath = join(root, "package.json");
function prepare() {
  if (!manifest) {
    run(["package", "--file", "browser-demo.sty", "--tlpdb", tlpdb, "--store", store, "--output", manifestPath]);
    manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    stats.preparations++;
  }
  return manifest;
}
const module = readFileSync(join(repo, "adapters/browser/filesystem.mjs"), "utf8").replace('"@noble/hashes/sha2.js"', '"/noble/sha2.js"');
const noble = dirname(process.argv[4] ?? createRequire(pathToFileURL(join(repo, "adapters/browser/package.json"))).resolve("@noble/hashes/sha2.js"));
const worker = `import {openPackageFile} from '/adapter.mjs';
self.onmessage=async ({data})=>{try{
 const {id,action,url,identity}=data;
 if(action!=='read'){
  const directory=await (await navigator.storage.getDirectory()).getDirectoryHandle('pqty.package-files.v1',{create:true});
  const name=identity.digest.slice(7);
  if(action==='remove')await directory.removeEntry(name).catch(error=>{if(error.name!=='NotFoundError')throw error;});
  else if(action==='corrupt'){const output=await (await directory.getFileHandle(name)).createWritable();await output.write('corrupt cached bytes');await output.close();}
  else if(action==='exists'){postMessage({id,value:await directory.getFileHandle(name).then(()=>true,error=>{if(error.name==='NotFoundError')return false;throw error;})});return;}
  postMessage({id,value:true});return;
 }
 const reader=await openPackageFile(url,identity);
 try{
  const memory=new WebAssembly.Memory({initial:Math.max(1,Math.ceil(identity.size/65536))});
  const target=new Uint8Array(memory.buffer,0,identity.size);
  let offset=0;
  while(offset<target.length){const count=reader.readInto(target.subarray(offset),offset);if(!count)throw new Error('Incomplete native read');offset+=count;}
  postMessage({id,value:new TextDecoder().decode(target)});
 }finally{reader.close();}
}catch(error){postMessage({id:data.id,error:String(error)})}};`;
const server = createServer((request, response) => {
  try {
    assert.equal(request.method, "GET", "no project or compile upload is allowed");
    const url = new URL(request.url, "http://localhost");
    if (url.pathname === "/") {
      response.setHeader("Content-Type", "text/html");
      response.end(`<script>
      const worker=new Worker('/worker.mjs',{type:'module'}),pending=new Map();let id=0;
      worker.onmessage=({data})=>{const p=pending.get(data.id);pending.delete(data.id);data.error?p.reject(new Error(data.error)):p.resolve(data.value)};
      const call=(action,url,identity)=>new Promise((resolve,reject)=>{pending.set(++id,{resolve,reject});worker.postMessage({id,action,url,identity})});
      window.readFile=(url,identity)=>call('read',url,identity);
      window.control=(action,identity)=>call(action,'',identity);
      </script>`);
    } else if (url.pathname === "/worker.mjs") {
      response.setHeader("Content-Type", "text/javascript");
      response.end(worker);
    } else if (/^\/noble\/[a-zA-Z0-9._-]+\.js$/.test(url.pathname)) {
      response.setHeader("Content-Type", "text/javascript");
      response.end(readFileSync(join(noble, url.pathname.slice("/noble/".length))));
    } else if (url.pathname === "/adapter.mjs") {
      response.setHeader("Content-Type", "text/javascript");
      response.end(module);
    } else if (url.pathname === "/package") {
      response.setHeader("Content-Type", "application/json");
      response.end(JSON.stringify(prepare()));
    } else if (/^\/objects\/[a-f0-9]{64}$/.test(url.pathname)) {
      const digest = `sha256:${url.pathname.slice("/objects/".length)}`;
      const file = prepare().files.find(file => file.digest === digest);
      assert(file, "unknown digest");
      assert.equal(file.tds_path, style, "unopened package file was transferred");
      const output = join(publicFiles, digest.slice(7));
      if (!existsSync(output)) {
        run(["package-file", "--manifest", manifestPath, "--store", store, "--path", file.tds_path, "--output", output]);
        stats.exports++;
      }
      const bytes = readFileSync(output);
      stats.fileRequests++;
      stats.fileBytes += bytes.length;
      response.setHeader("Content-Type", "application/octet-stream");
      response.setHeader("Cache-Control", "public, max-age=31536000, immutable");
      response.end(bytes);
    } else if (["/short", "/long", "/wrong"].includes(url.pathname)) {
      response.setHeader("Cache-Control", "no-store");
      const wrong = Buffer.alloc(Buffer.byteLength(text), 120);
      response.end(url.pathname === "/short" ? text.slice(1) : url.pathname === "/long" ? text + "x" : wrong);
    } else { response.writeHead(404); response.end(); }
  } catch (error) { response.writeHead(500); response.end(String(error)); }
});
let context;
try {
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  const open = async () => {
    context = await chromium.launchPersistentContext(profile, { executablePath, headless: true, args: ["--no-sandbox"], timeout: 30_000 });
    const page = await context.newPage();
    // Isolate OPFS from Chromium's separate HTTP cache. Otherwise an
    // offline repair can legitimately recover from a verified HTTP-cache hit.
    const network = await context.newCDPSession(page);
    await network.send("Network.enable");
    await network.send("Network.setCacheDisabled", { cacheDisabled: true });
    await page.goto(base);
    await page.waitForFunction(() => typeof window.readFile === "function");
    return page;
  };
  let page = await open();
  const packageManifest = await page.evaluate(() => fetch("/package").then(response => response.json()));
  assert.equal(packageManifest.schema, "pqty.package/v1");
  assert.equal(packageManifest.files.length, 2);
  const identity = packageManifest.files.find(file => file.tds_path === style);
  const url = `${base}/objects/${identity.digest.slice(7)}`;
  const read = () => page.evaluate(({ url, identity }) => window.readFile(url, identity), { url, identity });
  assert.equal(await read(), text);
  assert.equal(await read(), text);
  assert.equal(stats.fileRequests, 1);
  const firstTransfer = stats.fileBytes;
  await context.close(); context = undefined;
  page = await open();
  await context.setOffline(true);
  assert.equal(await read(), text, "disk cache must survive a complete Chromium restart");
  assert.equal(stats.fileRequests, 1);
  await page.evaluate(identity => window.control("corrupt", identity), identity);
  await assert.rejects(read(), "offline corrupt content must fail");
  assert.equal(await page.evaluate(identity => window.control("exists", identity), identity), false, "corrupt entries must be removed");
  await context.setOffline(false);
  assert.equal(await read(), text, "corrupt cache must recover the same pinned file");
  for (const path of ["/short", "/long", "/wrong"]) {
    await page.evaluate(identity => window.control("remove", identity), identity);
    const failed = await page.evaluate(async ({ url, identity }) => {
      try { await window.readFile(url, identity); return false; } catch { return true; }
    }, { url: `${base}${path}`, identity });
    assert.equal(failed, true, `${path} data must not enter the cache`);
    assert.equal(await page.evaluate(identity => window.control("exists", identity), identity), false, "failed downloads must not leave a cache entry");
  }
  assert.equal(stats.preparations, 1);
  assert.equal(stats.exports, 1);
  assert.equal(firstTransfer, Buffer.byteLength(text));
  assert.equal(existsSync(join(publicFiles, packageManifest.files.find(file => file.tds_path === extra).digest.slice(7))), false);
  assert.equal(existsSync(join(root, "main.tex")), false);
  console.log(JSON.stringify({ result: "passed", schema: "pqty.package/v1", initialFileRequests: 1, restartOfflineFileRequests: 0, bytesInFirstFetch: firstTransfer, preparations: stats.preparations, exports: stats.exports, checks: ["only requested file transferred", "native OPFS reads into WASM memory", "persistent restart/offline reuse", "offline corruption rejection", "same-identity recovery", "short/long/wrong-content rejection and cleanup", "no project source or compiler RPC"] }, null, 2));
} finally {
  await context?.close();
  server.closeAllConnections();
  await new Promise(resolve => server.close(resolve));
  rmSync(root, { recursive: true, force: true });
}
