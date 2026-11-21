// Real local stdio MCP acceptance against a synthetic Node project only.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {mkdtemp,mkdir,writeFile,rm,readFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {createInterface} from 'node:readline';
const executable=path.resolve('target/debug/verifact-cli');
const temporary=await mkdtemp(path.join(tmpdir(),'verifact-mcp-'));
const data=path.join(temporary,'data');
const project=path.join(temporary,'project');
const report={startedAt:new Date().toISOString(),checks:[]};
function cli(args){return JSON.parse(execFileSync(executable,['--data-dir',data,...args],{encoding:'utf8',timeout:30000}));}
function client(allowed){
 const child=spawn(executable,['--data-dir',data,'mcp',...(allowed?['--allow-execution',allowed]:[])],{stdio:['pipe','pipe','pipe']});
 const pending=new Map();let id=0;let stderr='';
 child.stderr.on('data',bytes=>{stderr+=bytes;if(stderr.length>65536)stderr=stderr.slice(-65536);});
 const lines=createInterface({input:child.stdout});
 lines.on('line',line=>{const response=JSON.parse(line);const entry=pending.get(response.id);if(entry){clearTimeout(entry.timer);pending.delete(response.id);entry.resolve(response);}});
 child.on('exit',(code)=>{for(const entry of pending.values()){clearTimeout(entry.timer);entry.reject(new Error(`MCP exited ${code}: ${stderr}`));}pending.clear();});
 return {child,request(method,params={}){const requestId=++id;return new Promise((resolve,reject)=>{const timer=setTimeout(()=>{pending.delete(requestId);reject(new Error(`MCP timeout: ${method}`));},30000);pending.set(requestId,{resolve,reject,timer});child.stdin.write(JSON.stringify({jsonrpc:'2.0',id:requestId,method,params})+'\n');});},async close(){child.stdin.end();if(child.exitCode===null)await new Promise(resolve=>child.once('exit',resolve));}};
}
let current;
try{
 await mkdir(project,{recursive:true});
 await writeFile(path.join(project,'package.json'),JSON.stringify({name:'example-node-tests',type:'module',scripts:{test:'node --test'}}));
 await writeFile(path.join(project,'arithmetic.test.mjs'),"import test from 'node:test';import assert from 'node:assert/strict';test('sum',()=>assert.equal(2+2,4));\n");
 const imported=cli(['add',project]);const projectId=imported.project.id;
 current=client();
 const early=await current.request('tools/list');assert.equal(early.error.code,-32000);
 const initialized=await current.request('initialize',{protocolVersion:'2025-11-25',capabilities:{},clientInfo:{name:'verifact-fixture-client',version:'1'}});
 assert.equal(initialized.result.serverInfo.name,'verifact');
 const tools=(await current.request('tools/list')).result.tools;
 assert.ok(tools.some(tool=>tool.name==='test_run'));
 const listed=await current.request('tools/call',{name:'test_list',arguments:{project_id:projectId,limit:1}});
 assert.equal(listed.result.structuredContent.total,1);
 const blocked=await current.request('tools/call',{name:'test_run',arguments:{project_id:projectId}});
 assert.equal(blocked.result.isError,true);assert.match(blocked.result.content[0].text,/not authorized/);
 assert.equal(cli(['runs',projectId]).length,0);
 report.checks.push('Protocol initialization, tools, bounded listing and denied unapproved execution');
 await current.close();current=client(projectId);
 await current.request('initialize',{protocolVersion:'2025-11-25'});
 const planned=await current.request('tools/call',{name:'test_plan',arguments:{project_id:projectId}});
 assert.equal(planned.result.isError,false);assert.equal(planned.result.structuredContent.command_count,1);
 const started=await current.request('tools/call',{name:'test_run',arguments:{project_id:projectId}});
 assert.equal(started.result.isError,false,JSON.stringify(started));
 const runId=started.result.structuredContent.id;
 let result;
 for(let i=0;i<200;i++){
  result=await current.request('tools/call',{name:'test_result',arguments:{run_id:runId}});
  if(!['queued','preparing','running'].includes(result.result.structuredContent.status))break;
  await new Promise(resolve=>setTimeout(resolve,25));
 }
 const record=result.result.structuredContent;
 assert.equal(record.status,'passed',JSON.stringify(record));assert.ok(record.results.some(item=>item.name==='sum'&&item.status==='passed'));
 const evidence=await current.request('tools/call',{name:'test_evidence',arguments:{run_id:runId}});
 assert.ok(evidence.result.structuredContent.artifacts.length>0);
 const output=await current.request('tools/call',{name:'test_output',arguments:{run_id:runId}});
 assert.ok(output.result.structuredContent.text.length>0);
 const history=await current.request('tools/call',{name:'test_history',arguments:{project_id:projectId}});
 assert.equal(history.result.structuredContent.runs[0].id,runId);
 report.checks.push('Operator-authorized real Node execution, parsed assertion, history and evidence through same core');
 await current.close();current=null;
 report.status='passed';
 console.log(`MCP acceptance passed: ${report.checks.length} groups`);
}catch(error){report.status='failed';report.error=error.stack;throw error;}
finally{await current?.close();report.finishedAt=new Date().toISOString();await mkdir('evidence',{recursive:true});await writeFile('evidence/mcp-verification.json',JSON.stringify(report,null,2)+'\n');await rm(temporary,{recursive:true,force:true});}
