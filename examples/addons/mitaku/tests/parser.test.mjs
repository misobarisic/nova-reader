import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';
import assert from 'node:assert/strict';
import {parseHTML} from 'linkedom';
const read = name => readFileSync(new URL(name, import.meta.url), 'utf8');
let body = read('fixtures/list.html'), status = 200, requested, finalUrl;
const context = {nova: {url: {resolve: (relative, base) => new URL(relative, base).href},
 http: {get(url, headers) {requested = {url, headers: JSON.parse(headers)}; return JSON.stringify({status, body, url: finalUrl || url});}},
 html: {select(html, selector, fields) {
  const {document} = parseHTML('<html><body>' + html + '</body></html>');
  return JSON.stringify([...document.querySelectorAll(selector)].map(element => Object.fromEntries(Object.entries(JSON.parse(fields)).map(([name, field]) => {
   const node = field.selector ? element.querySelector(field.selector) : element;
   const value = !node ? '' : field.value === 'html' ? node.innerHTML : field.value === 'ownText' ? [...node.childNodes].filter(n => n.nodeType === 3).map(n => n.textContent).join(' ').trim() : !field.value || field.value === 'text' ? node.textContent.trim() : node.getAttribute(field.value) || '';
   return [name, value];
  }))));
 }}}};
runInNewContext(read('../main.js'), context);
const call = request => JSON.parse(JSON.stringify(context.novaProvider.handle(request)));
const source = 'mitaku', base = 'https://mitaku.net', galleryId = '/gallery/clockwork';
assert.equal(call({op:'capabilities'}).sources[0].nsfw, true);
assert.equal(call({op:'capabilities'}).sources[0].id, source);
assert.equal(call({op:'browse',sourceId:'unknown'}).error.code,'NotFound');
let result = call({op:'browse',catalogue: 'popular',page:2});
assert.equal(result.items[0].id,galleryId); assert.equal(result.items[0].title,'Clockwork Garden'); assert.equal(result.items[0].coverUrl,base+'/images/cover.svg'); assert.equal(result.hasNextPage,true);
assert.equal(requested.headers.Referer,base+'/'); assert.equal(requested.headers.Origin,base);
body=read('fixtures/details.html'); result=call({op:'details',id:galleryId+'/'});
assert.equal(result.id,galleryId); assert.equal(result.title,'Clockwork Garden'); assert.equal(result.status,'completed'); assert.deepEqual(result.genres,['Art','Paper Models']);
result=call({op:'chapters',seriesId:galleryId}).chapters; assert.equal(result.length,1); assert.equal(result[0].id,galleryId); assert.equal(result[0].seriesId,galleryId); assert.equal(result[0].title,'Gallery'); assert.equal(result[0].uploadedAt,'2026-10-04');
result=call({op:'pages',chapterId:galleryId}).pages;
assert.deepEqual(result.map(page=>page.url),[base+'/images/full-2.svg',base+galleryId.slice(0,galleryId.lastIndexOf('/')+1)+'images/full-1.svg']);
assert.deepEqual(result.map(page=>page.id),['0','1']); assert.deepEqual(result[0].headers,{Referer:base+galleryId,Origin:base});
body=read('fixtures/video.html'); assert.match(call({op:'pages',id:galleryId}).error.message,/Video-only/);
for (const [http,code] of [[401,'AuthenticationRequired'],[403,'ChallengeRequired'],[404,'NotFound'],[429,'RateLimited'],[500,'Network']]) {status=http; assert.equal(call({op:'browse',catalogue:'popular'}).error.code,code);}
status=200;body='<html>Just a moment</html>';assert.equal(call({op:'browse',catalogue:'popular'}).error.code,'ChallengeRequired');
body='<div>Changed markup</div>';for(const op of ['browse','details','chapters','pages']) assert.equal(call({op,catalogue:'popular',id:galleryId}).error.code,'ParseChanged');
body=read('fixtures/empty.html');assert.deepEqual(call({op:'browse',catalogue:'popular'}),{items:[],hasNextPage:false});
body=read('fixtures/list.html').replace('https://mitaku.net','http://mitaku.net').replace('src="/images/cover.svg"','src="http://mitaku.net/images/cover.svg"');assert.equal(call({op:'browse',catalogue:'popular'}).error.code,'ParseChanged');
body=read('fixtures/list.html');
call({op:'browse',page:3});assert.equal(requested.url,base+'/category/ero-cosplay/page/3');
assert.equal(call({op:'browse',catalogue:'latest'}).error.code,'NotFound');
call({op:'search',query:' clock & paper ',page:2,filters:{category:'nude',tag:'paper'}});assert.equal(requested.url,base+'/page/2?s=clock%20%26%20paper');
call({op:'search',page:2,filters:{category:'sexy-set',tag:'paper'}});assert.equal(requested.url,base+'/category/sexy-set/page/2');
call({op:'search',filters:{tag:'  Paper   Models  '}});assert.equal(requested.url,base+'/tag/paper-models/page/1');
assert.equal(call({op:'filters'}).filters.length,2);
body=read('fixtures/details.html');finalUrl=base+galleryId+'/';result=call({op:'search',query:'clock'});assert.equal(result.items[0].id,galleryId);assert.equal(result.hasNextPage,false);finalUrl=null;
body=read('fixtures/details.html').replace('href="images/full-1.svg"','');assert.equal(call({op:'pages',id:galleryId}).error.code,'ParseChanged');

// A malformed later item must reject the whole listing.
body=read('fixtures/list.html');
body=body.replace('</div><div class="wp-pagenavi">','<article><h2>Missing link</h2></article></div><div class="wp-pagenavi">');
assert.equal(call({op:'browse',catalogue:'popular'}).error.code,'ParseChanged');
body=read('fixtures/details.html').replace('Clockwork Garden','Just a Moment: Verify You Are Human');assert.equal(call({op:'details',id:galleryId}).title,'Just a Moment: Verify You Are Human');
console.log('Mitaku neutral parser fixtures passed');
