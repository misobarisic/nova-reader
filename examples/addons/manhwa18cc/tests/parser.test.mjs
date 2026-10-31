import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';
import assert from 'node:assert/strict';
import {parseHTML} from 'linkedom';
const read = name => readFileSync(new URL(name, import.meta.url), 'utf8');
let body = read('fixtures/list.html'), status = 200, requested;
const context = {nova: {url: {resolve: (relative, base) => new URL(relative, base).href},
 http: {get(url, headers) {requested = {url, headers: JSON.parse(headers)}; return JSON.stringify({status, body, url});}},
 html: {select(html, selector, fields) {
  const {document} = parseHTML('<html><body>' + html + '</body></html>');
  return JSON.stringify([...document.querySelectorAll(selector)].map(element => Object.fromEntries(Object.entries(JSON.parse(fields)).map(([name, f]) => {
   const node = f.selector ? element.querySelector(f.selector) : element;
   const value = !node ? '' : f.value === 'html' ? node.innerHTML : f.value === 'ownText' ? [...node.childNodes].filter(n => n.nodeType === 3).map(n => n.textContent).join(' ').trim() : !f.value || f.value === 'text' ? node.textContent.trim() : node.getAttribute(f.value) || '';
   return [name, value];
  }))));
 }}}};
runInNewContext(read('../yiimadara.js') + '\n' + read('../main.js'), context);
const call = r => JSON.parse(JSON.stringify(context.novaProvider.handle(r)));
assert.deepEqual(call({op:'capabilities'}).sources.map(s=>s.language), ['en','ko','all']);
assert.ok(call({op:'capabilities'}).sources.every(s=>s.nsfw));
let result = call({op:'browse'}); assert.equal(result.items.length,1); assert.equal(result.items[0].id,'/webtoon/clockwork'); assert.equal(result.items[0].coverUrl,'https://manhwa18.cc/covers/clockwork.svg'); assert.equal(result.hasNextPage,true); assert.match(requested.url,/webtoons\/1\?orderby=trending$/);
assert.equal(call({op:'browse',sourceId:'manhwa18cc.all'}).items.length,2);
assert.equal(call({op:'browse',sourceId:'manhwa18cc.ko'}).items.length,2); assert.match(requested.url,/raw\/1/);
call({op:'browse',catalogue:'latest',page:3}); assert.equal(requested.url,'https://manhwa18.cc/webtoons/3');
call({op:'search',query:"clock's",filters:{genre:'genre/ignored'}}); assert.match(requested.url,/search\?q=clock%E2%80%99s&page=1$/);
call({op:'search',filters:{genre:'genre/adventure',category:'completed',sort:'latest'}}); assert.equal(requested.url,'https://manhwa18.cc/genre/adventure/1?orderby=latest');
body = body.replace(/<div class="page-item">[\s\S]*?<\/div>/,''); result=call({op:'browse'}); assert.equal(result.items.length,0); assert.equal(result.hasNextPage,true);
body=read('fixtures/details.html'); result=call({op:'details',id:'/webtoon/clockwork'}); assert.equal(result.title,'Clockwork'); assert.deepEqual(result.authors,['Ada','Sam']); assert.deepEqual(result.genres,['Adventure','Webtoon']); assert.equal(result.status,'completed'); assert.equal(result.description,'Neutral story.\n\nSecond paragraph.\n\nAlternative names: The Clock');
result=call({op:'chapters',id:'/webtoon/clockwork'}).chapters; assert.deepEqual(result.map(c=>c.title),['Chapter 2.5','Special']); assert.deepEqual(result.map(c=>c.sourceOrder),[0,1]); assert.equal(result[0].number,'2.5'); assert.equal(result[1].number,null); assert.equal(result[0].uploadedAt,'2026-10-09'); assert.equal(result[1].uploadedAt,'2026-10-08');
body=read('fixtures/pages.html'); result=call({op:'pages',id:'/webtoon/clockwork/chapter-2.5'}).pages; assert.deepEqual(result.map(p=>p.url),['https://manhwa18.cc/webtoon/images/2.svg','https://manhwa18.cc/images/1.svg','https://manhwa18.cc/images/3.svg']); assert.equal(result[0].headers.Referer,'https://manhwa18.cc/webtoon/clockwork/chapter-2.5'); assert.equal(result[0].headers.Origin,'https://manhwa18.cc');
body='<a href="/genre/z">Z</a><a href="/genre/a">A</a><a href="/genre/a">A</a>'; assert.deepEqual(call({op:'filters'}).filters[0].options.map(x=>x.label),['All','A','Z']);
for(const [http,code] of [[403,'ChallengeRequired'],[429,'RateLimited'],[401,'AuthenticationRequired'],[404,'NotFound']]) {status=http; assert.equal(call({op:'browse'}).error.code,code);}
status=200; body='<html>Just a moment</html>'; assert.equal(call({op:'browse'}).error.code,'ChallengeRequired');
body='<div>Changed markup</div>'; for(const op of ['browse','details','chapters','pages']) assert.equal(call({op,id:'/webtoon/clockwork'}).error.code,'ParseChanged');
body='<div class="post-title"><h1>Minimal</h1></div><ul class="row-content-chapter"></ul>'; result=call({op:'details',id:'/webtoon/minimal'}); assert.equal(result.description,null); assert.equal(result.coverUrl,null); assert.deepEqual(result.authors,[]); assert.equal(result.status,'unknown'); assert.deepEqual(call({op:'chapters',id:'/webtoon/minimal'}).chapters,[]);
body='<div class="no-results">No results</div>'; assert.equal(call({op:'browse'}).items.length,0);
console.log('YiiMadara synthetic parser fixtures passed');
