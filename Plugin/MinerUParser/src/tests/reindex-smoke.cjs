'use strict';
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const binary = process.argv[2] || path.resolve(__dirname, '../target/debug/MinerUParser.exe');
const base = fs.mkdtempSync(path.join(os.tmpdir(), 'mineru-reindex-smoke-'));
const id = 'paper_smoke';
const fileDir = path.join(base, 'file/mineru', id);
const imageDir = path.join(base, 'image/mineru', id, 'images');
function run(request) {
  return new Promise((resolve, reject) => {
    const child = spawn(binary, [], {
      env: { ...process.env, PROJECT_BASE_PATH: base }, stdio: ['pipe','pipe','pipe'],
      windowsHide: true,
    });
    let stdout = '', stderr = '';
    const timeout = setTimeout(() => { child.kill(); reject(new Error('native timeout')); }, 15000);
    child.stdout.on('data', data => { stdout += data; });
    child.stderr.on('data', data => { stderr += data; });
    child.on('error', error => { clearTimeout(timeout); reject(error); });
    child.on('close', code => {
      clearTimeout(timeout);
      try {
        assert.equal(code, 0, stderr);
        resolve(JSON.parse(stdout));
      } catch (error) { reject(error); }
    });
    child.stdin.on('error', reject);
    child.stdin.end(JSON.stringify(request));
  });
}
(async () => {
  fs.mkdirSync(fileDir, {recursive:true});
  fs.mkdirSync(imageDir, {recursive:true});
  const index = {
    paper_id:id, title:'Smoke', file_name:'source.pdf', pages:1, text_chars:10,
    image_count:0, table_count:0, formula_count:0, md_path:'old-md', render_url:'old-render',
    outline:[], images:[], errors:[], warnings:[], created_at:'original',
    custom_extension:{keep:true}, source_path:'original-source',
  };
  const raw = JSON.stringify(index);
  fs.writeFileSync(path.join(fileDir,'index.json'),raw);
  fs.writeFileSync(path.join(fileDir,'state.json'),JSON.stringify({
    paper_id:id,state:'indexed',file_name:'source.pdf',batch_id:'',
    created_at:'original',updated_at:'original',
  }));
  fs.writeFileSync(path.join(fileDir,'full.md'),'![](images/a.png)');
  fs.writeFileSync(path.join(fileDir,'content_list.json'),'[]');
  fs.writeFileSync(path.join(imageDir,'a.png'),Buffer.from([1,2,3]));
  const snapshot = () => Object.fromEntries(['state.json','full.md','content_list.json']
    .map(name => [name,fs.readFileSync(path.join(fileDir,name)).toString('hex')]));
  const originals = snapshot();
  const preview = await run({command:'reindex',paper_id:id});
  assert.equal(preview.status,'success');
  assert.equal(preview.result.data.applied,false);
  assert.equal(fs.readFileSync(path.join(fileDir,'index.json'),'utf8'),raw);
  const item = preview.result.data.items[0];
  assert.equal(item.inventory_preview.appended,1);
  const request = {command:'reindex',paper_id:id,apply:true,expected_revision:item.index_revision};

  // A competing process/old lock must not be automatically taken over.
  const lock = path.join(fileDir,'index.json.writer.lock');
  fs.writeFileSync(lock,'simulated other writer',{flag:'wx'});
  const blocked = await run(request);
  assert.equal(blocked.status,'error');
  assert.match(blocked.error,/INDEX_LOCKED/);
  assert.equal(fs.readFileSync(path.join(fileDir,'index.json'),'utf8'),raw);
  assert.equal(fs.readFileSync(lock,'utf8'),'simulated other writer');
  fs.unlinkSync(lock);

  const pair = await Promise.all([run(request),run(request)]);
  const successes = pair.filter(result=>result.status==='success');
  const failures = pair.filter(result=>result.status==='error');
  assert.equal(successes.length,1,JSON.stringify(pair));
  assert.equal(failures.length,1);
  assert.match(failures[0].error,/INDEX_LOCKED|REVISION_CONFLICT/);
  const receipt = successes[0].result.data;
  assert.equal(receipt.applied,true);
  assert.equal(receipt.appended,1);
  assert.equal(fs.readFileSync(receipt.backup,'utf8'),raw);
  const after = JSON.parse(fs.readFileSync(path.join(fileDir,'index.json'),'utf8'));
  assert.deepEqual(after.custom_extension,index.custom_extension);
  assert.equal(after.images.length,1);
  assert.equal(after.created_at,'original');
  assert.equal(after.source_path,'original-source');
  assert.deepEqual(snapshot(),originals);
  const retry = await run(request);
  assert.equal(retry.status,'error');
  assert.match(retry.error,/REVISION_CONFLICT/);
  assert(!fs.existsSync(lock));
  assert.equal(fs.readdirSync(fileDir).filter(name=>name.includes('.tmp.')).length,0);
  assert.equal(fs.readdirSync(fileDir).filter(name=>name.includes('.backup.')).length,1);
  console.log('PASS: native preview/apply, backup, unknown fields, stale revision, two-process race, lock refusal, source immutability');
})().catch(error=>{ console.error(error); process.exitCode=1; })
  .finally(()=>fs.rmSync(base,{recursive:true,force:true}));