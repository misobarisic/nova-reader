/* Apache-2.0. Native port of pinned Keiyoushi MangaFire; see NOTICE.md. */
(function () {
  'use strict';
  const base = 'https://mangafire.to';
  const langs = ['en', 'es', 'es-419', 'fr', 'ja', 'pt', 'pt-BR'];
  const ops = ['browse', 'search', 'filters', 'details', 'chapters', 'pages'];
  const ratings = ['safe', 'suggestive', 'erotica', 'pornographic'];
  const sortValues = ['chapter_updated_at:desc','relevance:desc','created_at:desc','title:asc','title:desc','year:desc','year:asc','score:desc','views_7d:desc','views_30d:desc','views_total:desc','follows_total:desc'];
  const sortNames = ['Latest update','Best match','Recently added','Title (A–Z)','Title (Z–A)','Year (newest)','Year (oldest)','Highest rated','Most viewed · 7 days','Most viewed · 30 days','Most viewed · all time','Most followed'];
  function fail(code, message) { const e = new Error(message); e.code = code; throw e; }
  function expect(condition, message) { if (!condition) fail('ParseChanged', message); }
  function object(value) { return value && typeof value === 'object' && !Array.isArray(value); }
  function array(value, label) { expect(Array.isArray(value), label + ' missing or changed'); return value; }
  function text(value, label) { expect(typeof value === 'string' && value.trim(), label + ' missing'); return value.trim(); }
  function numberId(value, label) { const id = String(value); expect(/^\d+$/.test(id) && (typeof value !== 'number' || Number.isSafeInteger(value)), label + ' invalid'); return id; }
  function hid(value) {
    const raw = text(value, 'Series ID');
    let token = raw.replace(/^https:\/\/mangafire\.to/, '').replace(/\/$/, '').split('/').pop();
    // Upstream accepts both the older slug.hid and the current hid-slug URLs.
    token = token.includes('.') ? token.split('.').pop() : token.split('-')[0];
    expect(/^[A-Za-z0-9]+$/.test(token), 'Series identity changed'); return token;
  }
  function imageUrl(value) {
    const url = nova.url.resolve(text(value, 'Image URL'), base + '/');
    expect(/^https:\/\/(?:[a-z0-9-]+\.)*(?:mfcdn\.nl|mfcdn1\.xyz|mangafire\.to)\//i.test(url), 'Unrecognized image host; addon permission update required');
    return url;
  }
  function get(path, pairs = []) {
    const url = MangaFireVrf.signedUrl(base, path, pairs);
    const response = JSON.parse(nova.http.get(url, JSON.stringify({Accept: 'application/json', Referer: base + '/'})));
    if (response.error) fail(response.error.code || 'Network', response.error.message || 'Source request failed');
    if (response.status === 429) fail('RateLimited', 'MangaFire rate limit; retry later');
    if (response.status === 401) fail('AuthenticationRequired', 'MangaFire authentication required');
    if (response.status === 403) fail('ChallengeRequired', 'MangaFire requires an interactive challenge; browser handoff is unavailable');
    if (response.status === 404) fail('NotFound', 'MangaFire resource not found');
    if (response.status < 200 || response.status >= 300) fail('Network', 'MangaFire HTTP ' + response.status);
    let result; try { result = JSON.parse(response.body); } catch (_) {
      if (/cf-chl-|challenge-platform|verify you are human|just a moment/i.test(response.body)) fail('ChallengeRequired', 'MangaFire returned an interactive HTML challenge; browser handoff is unavailable');
      fail('ParseChanged', 'MangaFire API returned non-JSON markup');
    }
    expect(object(result), 'MangaFire API envelope changed');
    const errorCode = typeof result.error === 'string' ? result.error : result.error?.code;
    if (['captcha_required','challenge_required'].includes(errorCode)) fail('ChallengeRequired', 'MangaFire requires an interactive challenge; browser handoff is unavailable');
    if (result.error) fail('ParseChanged', 'MangaFire API rejected the request');
    return result;
  }
  function series(row, detailed = false) {
    expect(object(row), 'Series record changed');
    const identity = hid(row.hid);
    const slug = row.slug ? text(row.slug, 'Series slug') : '';
    expect(!slug || /^[a-zA-Z0-9_-]+$/.test(slug), 'Series slug changed');
    const item = {id: '/title/' + identity, title: text(row.title, 'Series title'), url: base + '/title/' + identity + (slug ? '-' + slug : ''), coverUrl: null};
    if (row.poster) {
      expect(object(row.poster), 'Poster record changed');
      const cover = row.poster.large || row.poster.medium || row.poster.small;
      if (cover) item.coverUrl = imageUrl(cover);
    }
    if (detailed) {
      const entities = (value, label) => value == null ? [] : array(value, label).map(x => { expect(object(x), label + ' record changed'); return text(x.title, label); });
      expect(row.type == null || typeof row.type === 'string', 'Series type changed');
      item.authors = entities(row.authors, 'Author');
      item.artists = entities(row.artists, 'Artist');
      item.genres = [row.type ? row.type[0].toUpperCase() + row.type.slice(1) : '', ...entities(row.genres, 'Genre'), ...entities(row.themes, 'Theme')].filter(Boolean);
      item.status = ({releasing:'ongoing',finished:'completed',on_hiatus:'on_hiatus',discontinued:'cancelled'})[row.status] || 'unknown';
      item.description = null;
      if (row.synopsisHtml) {
        const rows = JSON.parse(nova.html.select('<div>' + row.synopsisHtml + '</div>', 'div', JSON.stringify({value:{value:'text'}})));
        item.description = rows[0]?.value || null;
      }
    }
    return item;
  }
  function list(data) {
    const items = array(data.items, 'Title list').map(row => series(row));
    expect(data.meta == null || object(data.meta), 'Pagination changed');
    expect(data.meta?.hasNext == null || typeof data.meta.hasNext === 'boolean', 'Pagination flag changed');
    return {items, hasNextPage: data.meta?.hasNext || false};
  }
  function addSelections(pairs, input, key, allowed) {
    if (input == null) return;
    array(input, 'Filter ' + key).forEach(value => { expect(allowed.includes(value), 'Unknown filter ' + key); pairs.push([key,value]); });
  }
  function filters() {
    const data = MangaFireFilterData;
    return {notice:'Content ratings default to Safe in this native port. Genre/theme matching and author lookup follow upstream.', filters:[
      {id:'contentRatings',type:'multiselect',options:ratings.map(value=>({label:value,value}))},
      {id:'types',type:'multiselect',options:data.TypeFilter},
      {id:'genres',type:'tristate',options:data.GenreFilter},
      {id:'themes',type:'multiselect',options:data.ThemeFilter},
      {id:'statuses',type:'multiselect',options:data.StatusFilter},
      {id:'matchMode',type:'select',options:[{label:'AND',value:'and'},{label:'OR',value:'or'}]},
      ...['author','yearFrom','yearTo','minChapter'].map(id=>({id,type:'text'})),
      {id:'sort',type:'select',options:sortValues.map((value,index)=>({label:sortNames[index],value}))}
    ]};
  }
  function chapter(row, seriesId, language, sourceOrder, volume) {
    expect(object(row), 'Chapter record changed');
    const id = numberId(row.id, 'Chapter ID');
    const value = String(row.number);
    expect(/^\d+(?:\.\d+)?$/.test(value), 'Chapter number changed');
    const name = typeof row.name === 'string' ? row.name.trim() : '';
    const extracted = /\b(?:ch\.?|chapter|ep\.?|episode)\s?(\d+(?:\.\d+)?)/i.exec(name);
    const number = !volume && extracted ? extracted[1] : value;
    const title = volume ? 'Vol. ' + value + (name ? ' - ' + name : '') : name && extracted ? name : 'Ch. ' + value + (name ? ' - ' + name : '');
    const local = seriesId + (volume ? '/volume/' + id : '/' + id + '-chapter-' + value + '-' + language);
    let uploadedAt = null;
    if (row.createdAt != null) {
      expect(typeof row.createdAt === 'number' && Number.isFinite(row.createdAt), 'Chapter date changed');
      const stamp = new Date(row.createdAt * 1000); expect(Number.isFinite(stamp.getTime()), 'Invalid chapter date'); uploadedAt = stamp.toISOString();
    }
    return {id:local,seriesId,title,number,sourceOrder,url:base+local,language:({'es-la':'es-419','pt-br':'pt-BR'})[language] || language,scanlator:volume ? String(row.chapterCount) + ' chapters' : row.type || 'Unknown',uploadedAt};
  }
  function handle(r) {
    const op = r.op || r.operation;
    const language = (r.sourceId || 'mangafire.en').replace(/^mangafire\./, '');
    expect(langs.includes(language), 'Unknown MangaFire language');
    const langCode = ({'es-419':'es-la','pt-BR':'pt-br'})[language] || language;
    const settings = r.settings || {};
    if (op === 'capabilities') return {sources:langs.map(language=>({id:'mangafire.'+language,name:'MangaFire',language,nsfw:true,contentClassification:'mixed',operations:ops}))};
    if (op === 'filters') return filters();
    if (op === 'browse' || op === 'search') {
      const page = Number(r.page || 1); expect(Number.isSafeInteger(page) && page > 0, 'Invalid catalogue page');
      const pairs = [['languages[]',langCode],['page',String(page)],['limit','50']];
      const f = op === 'search' ? r.filters || {} : {};
      const rating = settings.contentRating || 'safe'; expect(rating === 'all' || ratings.includes(rating), 'Invalid content rating');
      addSelections(pairs, f.contentRatings == null ? rating === 'all' ? [] : [rating] : f.contentRatings, 'content_rating[]', ratings);
      if (op === 'browse') {
        const catalogue = r.catalogue || r.kind || 'popular'; expect(['popular','latest'].includes(catalogue), 'Unknown catalogue');
        pairs.push([catalogue === 'popular' ? 'order[views_30d]' : 'order[chapter_updated_at]','desc']);
      } else {
        if (r.query?.trim()) pairs.push(['keyword',r.query.trim()]);
        if (f.author?.trim()) {
          const tags = array(get('/api/tags', [['keyword',f.author.trim()]]).data, 'Author lookup');
          tags.forEach(tag => expect(object(tag), 'Author tag changed'));
          const tag = tags.find(t=>t.type === 'author' || t.type === 'artist');
          if (!tag) return {items:[],hasNextPage:false};
          pairs.push(['authors[]',numberId(tag.id,'Author ID')]);
        }
        const data = MangaFireFilterData;
        addSelections(pairs,f.types,'types[]',data.TypeFilter.map(x=>x.value));
        addSelections(pairs,f.genres?.include,'genres_in[]',data.GenreFilter.map(x=>x.value));
        addSelections(pairs,f.genres?.exclude,'genres_ex[]',data.GenreFilter.map(x=>x.value));
        addSelections(pairs,f.themes,'theme_ids[]',data.ThemeFilter.map(x=>x.value));
        addSelections(pairs,f.statuses,'statuses[]',data.StatusFilter.map(x=>x.value));
        const mode = f.matchMode || 'and'; expect(['and','or'].includes(mode), 'Unknown genre match mode');
        pairs.push(['genres_mode',mode],['theme_mode',mode]);
        for (const [key,param] of [['yearFrom','year_from'],['yearTo','year_to'],['minChapter','min_chap']]) {
          if (f[key] != null && f[key] !== '') { const number = Number(f[key]); if (Number.isSafeInteger(number) && (key !== 'minChapter' || number > 0)) pairs.push([param,String(number)]); }
        }
        const sort = f.sort || 'relevance:desc'; expect(sortValues.includes(sort),'Unknown search sort');
        const [field,direction] = sort.split(':'); pairs.push(['order['+field+']',direction]);
      }
      return list(get('/api/titles',pairs));
    }
    if (op === 'details') return series(get('/api/titles/'+hid(r.id || r.seriesId || r.url)).data,true);
    if (op === 'chapters') {
      const identity = hid(r.id || r.seriesId || r.url); const seriesId = '/title/'+identity;
      let rows = []; let volume = false;
      if (settings.preferVolumes) {
        const allVolumes = array(get('/api/titles/'+identity+'/volumes').items,'Volume list');
        if (allVolumes.length) { rows = allVolumes.filter(row=>row.language === langCode); volume = true; }
      }
      if (!volume) {
        let lastPage = 1;
        for (let page=1;page<=lastPage;page++) {
          const data = get('/api/titles/'+identity+'/chapters',[['language',langCode],['sort','number'],['order','desc'],['page',String(page)],['limit','200']]);
          if (page === 1) { lastPage = data.meta?.lastPage || 1; expect(Number.isSafeInteger(lastPage) && lastPage > 0 && lastPage <= (settings.preferVolumes ? 11 : 12), 'Chapter catalogue exceeds the host request budget; no partial chapter list returned'); }
          rows.push(...array(data.items,'Chapter list'));
        }
      }
      let chapters = rows.map((row,index)=>chapter(row,seriesId,langCode,index,volume));
      if (!volume && settings.mergeChapters) {
        const preference = settings.preferOfficial !== false;
        chapters.sort((a,b)=>Number((a.scanlator.toLowerCase() === 'official') !== preference)-Number((b.scanlator.toLowerCase() === 'official') !== preference));
        const seen = new Set(); chapters = chapters.filter(row=>!seen.has(row.number) && seen.add(row.number)).sort((a,b)=>Number(b.number)-Number(a.number));
      }
      const seenIds = new Set(); chapters.forEach((row,index)=>{expect(!seenIds.has(row.id),'Duplicate chapter identity');seenIds.add(row.id);row.sourceOrder=index;});
      return {chapters};
    }
    if (op === 'pages') {
      const raw = text(r.chapterId || r.id || r.url,'Chapter ID').replace(/^https:\/\/mangafire\.to/, '');
      const match = /^\/title\/[A-Za-z0-9]+(?:-[A-Za-z0-9_-]+)?\/(?:volume\/(\d+)|(\d+)-chapter-\d+(?:\.\d+)?-[a-zA-Z-]+)$/.exec(raw);
      expect(match, 'Refresh the chapter list; unsupported chapter identity');
      const volume = Boolean(match[1]); const id = match[1] || match[2];
      const data = get('/api/'+(volume?'volumes/':'chapters/')+id);
      const rows = array(data.data?.pages,'Reader pages'); expect(rows.length,'Reader page list is empty');
      return {pages:rows.map((row,index)=>{
        expect(object(row),'Reader page changed');
        const page = {id:String(index),url:imageUrl(row.url),headers:{Referer:base+raw}};
        if (Number.isSafeInteger(row.width) && row.width > 0) page.width=row.width;
        if (Number.isSafeInteger(row.height) && row.height > 0) page.height=row.height;
        return page;
      })};
    }
    fail('NotFound','Unsupported MangaFire operation');
  }
  globalThis.novaProvider = {handle(request) {try{return handle(request);}catch(e){if(!e.code)throw e;return {error:{code:e.code,message:e.message}};}}};
})();
