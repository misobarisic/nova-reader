// Neutral JSON records. No live metadata, image downloads or external dependencies.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';

const uid = n => '00000000-0000-4000-8000-' + String(n).padStart(12, '0');
const base = 'https://kagane.moe';
let requests = [], force404 = false, token = 0;
const context = {nova: {http: {request(input) {
  const request = JSON.parse(input); requests.push(request);
  const url = new URL(request.url);
  assert.equal(url.origin, base);
  assert.equal(request.headers.Origin, base);
  assert.equal(request.headers.Referer, base + '/');
  if (force404 && url.pathname !== '/') return JSON.stringify({status:404,url:request.url,body:'{}'});
  let body;
  switch (url.pathname) {
    case '/': return JSON.stringify({status:200,url:base+'/',body:'<html></html>'});
    case '/api/v2/search/series': body={last:false,content:[{series_id:uid(1),title:'Clockmaker',cover_image_id:uid(2)}]}; break;
    case '/api/v2/series/'+uid(1): body={title:'Clockmaker',format:'Manhwa',upload_status:'Ongoing'}; break;
    case '/api/v2/series/'+uid(1)+'/books/index': body={count:2,fields:['book_id','sort_no','chapter_no','volume_no','group_ix','released_at','title','page_count'],groups:[],books:[[uid(3),1,'1','',[],null,'First',2],[uid(4),2,'1.5','',[],null,'Interlude',2]]}; break;
    case '/api/v2/genres/list': body=[{id:uid(8),genre_name:'Adventure'}]; break;
    case '/api/v2/tags/list': body=[]; break;
    case '/api/v2/sources/list': body={sources:[{source_id:uid(9),title:'Sample Press',source_type:'Official'}]}; break;
    case '/api/integrity': body={token:'synthetic-integrity'}; break;
    case '/api/v2/books/'+uid(3):
      assert.equal(request.headers['x-integrity-token'],'synthetic-integrity');
      body={access_token:'synthetic-'+token++,cache_url:'https://kstatic.to',manifest:{pages:[{page_no:1,page_id:uid(6),ext:'jxl'},{page_no:0,page_id:uid(5),ext:'jxl'}]}}; break;
    default: throw new Error('Unexpected synthetic route ' + url.pathname);
  }
  return JSON.stringify({status:200,url:request.url,body:JSON.stringify(body)});
}}}};
runInNewContext(readFileSync(new URL('../main.js',import.meta.url),'utf8'),context);
const call = request => JSON.parse(JSON.stringify(context.novaProvider.handle(request)));
const sources=call({op:'capabilities'}).sources;
assert.equal(sources.length,18);
for(const source of sources) {
  const result=call({op:'browse',sourceId:source.id});
  assert.equal(result.items[0].id,uid(1));
  assert.deepEqual(JSON.parse(requests.at(-1).body).content_lang,[source.language]);
}
call({op:'search',query:'Clock',page:2}); assert.match(requests.at(-1).url,/page=1/);
assert.equal(call({op:'details',seriesId:uid(1)}).id,uid(1));
assert.deepEqual(call({op:'chapters',seriesId:uid(1)}).chapters.map(c=>c.number),['1.5','1']);
assert.ok(call({op:'filters'}).filters.length>5);
const first=call({op:'pages',chapterId:uid(3)}).pages;
assert.equal(first.length,2); assert.equal(first[0].id,uid(5));
assert.equal(first[0].headers.Origin,base); assert.equal(first[0].headers.Referer,base+'/');
assert.match(first[0].url,/\.jxl\?token=/);
const refreshed=call({op:'resolvePage',url:first[0].url}).pages;
assert.equal(refreshed.length,1); assert.equal(refreshed[0].id,first[0].id); assert.notEqual(refreshed[0].url,first[0].url);
assert.equal(refreshed[0].headers.Origin,base);
const saver=call({op:'pages',chapterId:uid(3),settings:{dataSaver:true}}).pages;
assert.match(saver[0].url,/\/page\/datasaver\//);
assert.deepEqual(saver.map(p=>p.id),first.map(p=>p.id));
force404=true;
for(const op of ['browse','search','filters','pages']) assert.equal(call({op,chapterId:uid(3)}).error.code,'ParseChanged');
for(const op of ['details','chapters']) assert.equal(call({op,seriesId:uid(1)}).error.code,'NotFound');
assert.equal(call({op:'browse',settings:{mirror:'kagane.cx'}}).error.code,'ParseChanged');
assert.ok(requests.every(r=>new URL(r.url).origin===base));
console.log('Kagane neutral request/header fixtures passed');
