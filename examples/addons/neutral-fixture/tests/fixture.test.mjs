import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';
import assert from 'node:assert/strict';
const context = {};
runInNewContext(readFileSync(new URL('../main.js', import.meta.url), 'utf8'), context);
const call = r => JSON.parse(JSON.stringify(context.novaProvider.handle(r)));
assert.equal(call({op:'capabilities'}).sources[0].nsfw,false);
assert.equal(call({op:'browse'}).items.length,1);
assert.equal(call({op:'search',query:'shapes'}).items.length,1);
assert.equal(call({op:'browse',page:2}).items.length,0);
assert.equal(call({op:'chapters'}).chapters.length,5);
const pages=call({op:'pages',chapterId:'/fixture/hundreds'}).pages;
assert.equal(pages.length,320);
assert.equal(new Set(pages.map(p=>p.id)).size,320);
for(const p of pages) {
 assert.deepEqual(p.headers,{});
 const path=p.url.replace('addon://neutral-fixture/','');
 const png=readFileSync(new URL('../'+path,import.meta.url));
 assert.equal(png.readUInt32BE(16),p.width);
 assert.equal(png.readUInt32BE(20),p.height);
}
assert.equal(call({op:'pages',chapterId:'/fixture/unknown'}).error.code,'NotFound');
console.log('Neutral fixture source and bundled PNG dimensions passed');
