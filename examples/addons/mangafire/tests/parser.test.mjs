import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';
import assert from 'node:assert/strict';
const read = name => readFileSync(new URL(name,import.meta.url),'utf8');
let response={status:200,body:read('fixtures/list.json')}, requests=[];
const context={nova:{url:{resolve:(relative,base)=>new URL(relative,base).href},http:{get(url,headers){requests.push({url,headers:JSON.parse(headers)});return JSON.stringify({...response,url});}},html:{select(html){return JSON.stringify([{value:html.replace(/<[^>]*>/g,'').trim()}]);}}}};
runInNewContext(['vrf.js','filter-data.js','main.js'].map(file=>read('../'+file)).join('\n;\n'),context);
const call=r=>JSON.parse(JSON.stringify(context.novaProvider.handle(r)));
assert.equal(context.MangaFireVrf.sign('/titles'),'8sK3xtqdFQ');
assert.equal(context.MangaFireVrf.sign('/titles?keyword=café&languages[0]=en'),'8sK3xtqdFZfetBhus6bRAkzmMZAN2NSgrIL7x1-rtK9CmkLVYw');
let result=call({op:'capabilities'});assert.deepEqual(result.sources.map(s=>s.language),['en','es','es-419','fr','ja','pt','pt-BR']);assert.ok(result.sources.every(s=>s.nsfw));
result=call({op:'browse',page:2});assert.equal(result.items[0].id,'/title/clock1');assert.equal(result.hasNextPage,true);assert.equal(result.items[0].coverUrl,'https://static.mfcdn.nl/covers/large.png');let url=new URL(requests.at(-1).url);assert.equal(url.searchParams.get('page'),'2');assert.equal(url.searchParams.get('order[views_30d]'),'desc');assert.equal(url.searchParams.get('content_rating[]'),'safe');assert.ok(url.searchParams.get('vrf'));
call({op:'browse',catalogue:'latest',sourceId:'mangafire.es-419'});url=new URL(requests.at(-1).url);assert.equal(url.searchParams.get('languages[]'),'es-la');assert.equal(url.searchParams.get('order[chapter_updated_at]'),'desc');
call({op:'search',query:' café ',sourceId:'mangafire.pt-BR',filters:{genres:{include:['1'],exclude:['78']},types:['manga'],matchMode:'or',sort:'title:asc',minChapter:'2'}});url=new URL(requests.at(-1).url);assert.equal(url.searchParams.get('keyword'),'café');assert.equal(url.searchParams.get('languages[]'),'pt-br');assert.equal(url.searchParams.get('genres_in[]'),'1');assert.equal(url.searchParams.get('genres_ex[]'),'78');assert.equal(url.searchParams.get('order[title]'),'asc');
response.body=read('fixtures/details.json');result=call({op:'details',id:'/title/clock1'});assert.equal(result.id,'/title/clock1');assert.match(result.url,/renamed-clockwork-garden$/);assert.equal(result.status,'on_hiatus');assert.deepEqual(result.authors,['Ada']);assert.deepEqual(result.genres,['Manga','Adventure','Time Travel']);
response.body=read('fixtures/pages.json');result=call({op:'pages',chapterId:'/title/clock1/101-chapter-2.5-en'});assert.equal(result.pages[0].id,'0');assert.match(result.pages[0].url,/second.png/);assert.equal(result.pages[1].height,12000);assert.equal(result.pages[0].headers.Referer,'https://mangafire.to/title/clock1/101-chapter-2.5-en');
for(const [status,code]of [[403,'ChallengeRequired'],[429,'RateLimited'],[401,'AuthenticationRequired'],[404,'NotFound'],[500,'Network']]){response.status=status;assert.equal(call({op:'browse'}).error.code,code);}
response.status=200;response.body=read('fixtures/challenge-phrase.json');assert.equal(call({op:'browse'}).items[0].title,'Just a Moment: Verify You Are Human');response.body='{"error":"captcha_required"}';assert.equal(call({op:'browse'}).error.code,'ChallengeRequired');response.body='<html>changed</html>';assert.equal(call({op:'browse'}).error.code,'ParseChanged');response.body='{"items":[],"meta":{"hasNext":true}}';assert.equal(call({op:'browse'}).hasNextPage,true);
console.log('MangaFire signer/API synthetic fixtures passed');
