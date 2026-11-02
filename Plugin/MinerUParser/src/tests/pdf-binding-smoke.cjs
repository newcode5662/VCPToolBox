'use strict';
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const binary = process.argv[2] || path.resolve(__dirname, '../target/debug/MinerUParser.exe');
const base = fs.mkdtempSync(path.join(os.tmpdir(), 'mineru-pdf-smoke-'));
function run(request) {
  return new Promise((resolve, reject) => {
    const child = spawn(binary, [], { windowsHide:true,
      env:{...process.env,PROJECT_BASE_PATH:base},stdio:['pipe','pipe','pipe'] });
    let stdout='', stderr='';
    const timer=setTimeout(()=>{child.kill();reject(Error('native timeout'));},15000);
    child.stdout.on('data',data=>{stdout+=data;});
    child.stderr.on('data',data=>{stderr+=data;});
    child.on('error',error=>{clearTimeout(timer);reject(error);});
    child.on('close',code=>{
      clearTimeout(timer);
      try {assert.equal(code,0,stderr);resolve(JSON.parse(stdout));}
      catch(error){reject(error);}
    });
    child.stdin.on('error',reject);
    child.stdin.end(JSON.stringify(request));
  });
}
function data(result) {
  assert.equal(result.status,'success',JSON.stringify(result));
  return result.result.data;
}
(async()=>{
  const pdf=path.join(base,'source.pdf');
  const pdfBytes=Buffer.from('%PDF-1.4 native binding fixture');
  fs.writeFileSync(pdf,pdfBytes);
  const sha=crypto.createHash('sha256').update(pdfBytes).digest('hex');
  const id='paper_'+sha.slice(0,16);
  const dir=path.join(base,'file/mineru',id);
  fs.mkdirSync(dir,{recursive:true});
  const index={paper_id:id,title:'PDF smoke',file_name:'source.pdf',pages:1,text_chars:1,
    image_count:0,table_count:0,formula_count:0,md_path:'old',render_url:'old',
    images:[],outline:[],errors:[],warnings:[],created_at:'original',source_path:'original-source'};
  fs.writeFileSync(path.join(dir,'index.json'),JSON.stringify(index));
  fs.writeFileSync(path.join(dir,'state.json'),JSON.stringify({
    paper_id:id,state:'indexed',file_name:'source.pdf',batch_id:'',
    created_at:'original',updated_at:'original'}));
  const original=Object.fromEntries(['index.json','state.json'].map(n=>
    [n,fs.readFileSync(path.join(dir,n)).toString('hex')]));
  const binding=path.join(dir,'pdf-binding.json');
  const preview=data(await run({command:'link_pdf',paper_id:id,file_path:pdf}));
  assert.equal(preview.binding_revision,'missing');
  assert.equal(preview.sha256,sha);
  assert.equal(preview.can_apply,true);
  assert(!fs.existsSync(binding));
  const request={command:'link_pdf',paper_id:id,file_path:pdf,apply:true,
    expected_revision:'missing',expected_pdf_sha256:sha};
  const wrong=await run({...request,expected_pdf_sha256:'0'.repeat(64)});
  assert.equal(wrong.status,'error');
  const pair=await Promise.all([run(request),run(request)]);
  assert.equal(pair.filter(r=>r.status==='success').length,1,JSON.stringify(pair));
  assert.equal(pair.filter(r=>r.status==='error').length,1);
  const query=data(await run({command:'get_pdf_binding',paper_id:id}));
  assert.equal(query.binding.sha256,sha);
  assert.equal(query.binding.disabled,false);
  const replay=data(await run({...request,expected_revision:query.binding_revision}));
  assert.equal(replay.unchanged,true);
  const unlinkPreview=data(await run({command:'unlink_pdf',paper_id:id}));
  assert.equal(unlinkPreview.applied,false);
  const unlink=data(await run({command:'unlink_pdf',paper_id:id,apply:true,
    expected_revision:unlinkPreview.binding_revision}));
  assert.equal(unlink.disabled,true);
  const disabled=data(await run({command:'get_pdf_binding',paper_id:id}));
  assert.equal(disabled.binding.disabled,true);
  assert.equal(disabled.binding.path,undefined);
  assert(fs.existsSync(unlink.backup));
  const stale=await run({...request,expected_revision:query.binding_revision});
  assert.equal(stale.status,'error');
  const rebound=data(await run({...request,expected_revision:disabled.binding_revision}));
  assert.equal(rebound.binding.disabled,false);
  for(const [n,hex] of Object.entries(original))
    assert.equal(fs.readFileSync(path.join(dir,n)).toString('hex'),hex);
  assert.deepEqual(fs.readFileSync(pdf),pdfBytes);
  assert(!fs.existsSync(path.join(dir,'pdf-binding.json.writer.lock')));
  console.log('PASS: PDF native preview, two-process race, query, replay, unlink, stale revision, rebind, source preservation');
})().catch(error=>{console.error(error);process.exitCode=1;})
.finally(()=>fs.rmSync(base,{recursive:true,force:true}));