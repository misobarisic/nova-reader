// Newly authored neutral fixtures; no scraped site galleries or image downloads.
import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';
import assert from 'node:assert/strict';
import {parseHTML} from 'linkedom';
const read = name => readFileSync(new URL(name, import.meta.url), 'utf8');
let body = read('fixtures/list.html'), status = 200, cookies = [], requests = [], overrides = new Map();
const context = {nova: {
  url: {resolve: (relative, base) => new URL(relative, base).href},
  cookies: {set(origin, name, value) { cookies.push({origin, name, value}); return JSON.stringify({ok:true}); }},
  http: {request(input) {
    const r = JSON.parse(input); requests.push(r);
    return JSON.stringify({status, body:overrides.get(r.url) ?? body, url:r.url});
  }},
  html: {select(html, selector, fields) {
    const {document} = parseHTML('<html><body>' + html + '</body></html>');
    return JSON.stringify([...document.querySelectorAll(selector)].map(element => Object.fromEntries(Object.entries(JSON.parse(fields)).map(([name, f]) => {
      const node = f.selector ? element.querySelector(f.selector) : element;
      const value = !node ? '' : f.value === 'html' ? node.innerHTML : f.value === 'ownText' ? [...node.childNodes].filter(n => n.nodeType === 3).map(n => n.textContent).join(' ').trim() : !f.value || f.value === 'text' ? node.textContent.trim() : node.getAttribute(f.value) || '';
      return [name, value];
    }))));
  }}
}};
runInNewContext(read('../main.js'), context);
const call = r => JSON.parse(JSON.stringify(context.novaProvider.handle(r)));
const last = () => requests.at(-1);
const base = 'https://toonily.com';
assert.equal(call({op:'capabilities'}).sources[0].id,'toonily.en');
assert.equal(call({op:'capabilities'}).sources[0].nsfw,true);
assert.equal(cookies.length,0);
let result = call({op:'browse',page:2});
assert.equal(result.items[0].id,'/serie/clockwork-garden');
assert.equal(result.items[0].coverUrl,'https://static.tnlycdn.com/neutral/clockwork-175x238.png');
assert.equal(result.hasNextPage,true);
assert.equal(last().url,base + '/serie/page/2/?m_orderby=views');
assert.deepEqual(cookies.at(-1),{origin:base,name:'toonily-mature',value:'0'});
call({op:'browse',catalogue:'latest',settings:{showMature:true}});
assert.equal(last().url,base + '/serie/?m_orderby=latest'); assert.equal(cookies.at(-1).value,'1');
call({op:'search',page:3,query:"  Clock's GARDEN!! café ",filters:{author:' Ada ',artist:' Sam ',release:'2026',status:['end','on-hold'],sort:'rating',adult:'0',genreCondition:'1',genres:['clockwork','adventure']}});
assert.match(last().url,/\/search\/clock-s-garden-caf\/page\/3\//);
let query = new URL(last().url).searchParams;
assert.equal(query.get('author'),'Ada'); assert.equal(query.get('artist'),'Sam'); assert.equal(query.get('release'),'2026');
assert.deepEqual(query.getAll('status[]'),['end','on-hold']); assert.deepEqual(query.getAll('genre[]'),['clockwork','adventure']);
assert.equal(query.get('op'),'1'); assert.equal(query.get('adult'),'0'); assert.equal(query.get('m_orderby'),'rating');
call({op:'search',query:'Ignored',page:2,filters:{tag:'mechanical-garden',sort:'latest',genres:['ignored'],adult:'1',author:'Ignored'}});
assert.equal(last().url,base + '/tag/mechanical-garden/page/2/?m_orderby=latest');
call({op:'search',query:'!!!'}); assert.equal(last().url,base + '/search/');
body='<div class="no-results">Nothing found</div>'; result=call({op:'browse'}); assert.deepEqual(result.items,[]); assert.equal(result.hasNextPage,false);
body=read('fixtures/details.html'); result=call({op:'details',id:'/serie/clockwork-garden',url:'/webtoon/clockwork-garden/'});
assert.equal(result.id,'/serie/clockwork-garden'); assert.equal(result.title,'Clockwork Garden');
assert.deepEqual(result.authors,['Ada','Sam']); assert.deepEqual(result.genres,['Adventure','Clockwork','Webtoon']); assert.equal(result.status,'on_hiatus');
assert.equal(result.description,'A gardener repairs a mechanical greenhouse.\n\nA neutral synthetic story.\n\nAlternative names: The Clock Garden');
body=read('fixtures/chapters.html'); result=call({op:'chapters',id:'/serie/clockwork-garden'}).chapters;
assert.equal(last().url,base + '/serie/clockwork-garden/ajax/chapters/'); assert.equal(last().method,'POST'); assert.equal(last().body,'');
assert.equal(last().headers['X-Requested-With'],'XMLHttpRequest');
assert.deepEqual(result.map(x=>x.number),['2.5',null]); assert.deepEqual(result.map(x=>x.sourceOrder),[0,1]);
assert.equal(result[0].id,'/serie/clockwork-garden/chapter-2-5'); assert.equal(result[0].uploadedAt,'2026-10-09'); assert.equal(result[1].uploadedAt,'2026-10-08');
body='<ul class="main version-chap"></ul>'; assert.deepEqual(call({op:'chapters',id:'/serie/clockwork-garden'}).chapters,[]);
body=read('fixtures/pages.html'); result=call({op:'pages',id:'/serie/clockwork-garden/chapter-2-5'}).pages;
assert.equal(result.length,7); assert.equal(result[0].url,'https://data.tnlycdn.com/neutral/panel-1.svg?token=old');
assert.equal(result[5].url,base + '/neutral/panel-6.svg'); assert.equal(result[0].headers.Referer,base + '/serie/clockwork-garden/chapter-2-5/');
const ids = result.map(x=>x.id); body=body.replaceAll('token=old','token=new'); assert.deepEqual(call({op:'pages',id:'/serie/clockwork-garden/chapter-2-5'}).pages.map(x=>x.id),ids);
const chapter=base + '/serie/clockwork-garden/chapter-2-5/';
body='<div id="single-pager"></div>'; overrides.set(chapter+'?style=list',read('fixtures/pages.html'));
assert.equal(call({op:'pages',id:'/serie/clockwork-garden/chapter-2-5'}).pages.length,7); assert.equal(last().url,chapter+'?style=list');
overrides.clear(); assert.equal(call({op:'pages',id:'/serie/clockwork-garden/chapter-2-5'}).error.code,'ParseChanged');
body=read('fixtures/pages.html') + '<script id="chapter-protector-data">encrypted</script>'; assert.equal(call({op:'pages',id:'/serie/clockwork-garden/chapter-2-5'}).error.code,'ParseChanged');
body='<div class="reading-content"><div class="text-left"><img src="/1.svg"><img src="/2.svg"></div></div>'; assert.equal(call({op:'pages',id:'/serie/clockwork-garden/chapter-2-5'}).error.code,'ParseChanged');
overrides.set(base+'/serie/',read('fixtures/genres.html')); overrides.set(base+'/wp-manga-tag-sitemap.xml',read('fixtures/tags.xml'));
result=call({op:'filters'}); assert.deepEqual(result.filters.find(x=>x.id==='genres').options.map(x=>x.value),['adventure','clockwork']);
assert.deepEqual(result.filters.find(x=>x.id==='tag').options.map(x=>x.value),['','clock','mechanical-garden']);
overrides.set(base+'/wp-manga-tag-sitemap.xml','<urlset>'+Array.from({length:200},(_,i)=>`<url><loc>${base}/tag/tag-${i}/</loc></url>`).join('')+'</urlset>');
assert.equal(call({op:'filters'}).filters.find(x=>x.id==='tag').type,'text'); overrides.clear();
for(const [http,code] of [[401,'AuthenticationRequired'],[403,'ChallengeRequired'],[404,'NotFound'],[429,'RateLimited']]) {status=http; assert.equal(call({op:'browse'}).error.code,code);}
status=200; body='<html>Just a moment</html>'; assert.equal(call({op:'browse'}).error.code,'ChallengeRequired');
body='<div>Changed markup</div>'; for(const op of ['browse','details','chapters','pages','filters']) assert.equal(call({op,id:op==='pages'?'/serie/clockwork-garden/chapter-2-5':'/serie/clockwork-garden'}).error.code,'ParseChanged');
body='<div class="post-title"><h1>Minimal</h1></div>'; result=call({op:'details',id:'/serie/minimal'}); assert.equal(result.description,null); assert.equal(result.coverUrl,null); assert.deepEqual(result.authors,[]); assert.equal(result.status,'unknown');
for(const op of ['browse','search','filters','details','chapters','pages']) {
  const before=cookies.length; call({op,id:op==='pages'?'/serie/clockwork-garden/chapter-2-5':'/serie/clockwork-garden',settings:{showMature:true}}); assert.equal(cookies.length,before+1); assert.equal(cookies.at(-1).value,'1');
}
assert.equal(call({op:'browse',sourceId:'toonily.ko'}).error.code,'NotFound');
assert.equal(call({op:'details',id:'https://elsewhere.example/serie/clockwork-garden/'}).error.code,'ParseChanged');
assert.ok(requests.every(r=>!('Cookie' in r.headers))); assert.ok(cookies.every(c=>c.origin===base&&c.name==='toonily-mature'));
console.log('Toonily synthetic parser fixtures passed');
