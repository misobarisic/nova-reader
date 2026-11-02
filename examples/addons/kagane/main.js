/* Apache-2.0. Modified Keiyoushi Kagane v39 port; see NOTICE.md. */
(function () {
  'use strict';
  const languages = ['en','ja','ko','zh','es','es-419','fr','de','pt','pt-BR','ru','it','id','vi','th','pl','hi','ar'];
  const operations = ['browse','search','filters','details','chapters','pages','resolvePage'];
  // Former mirror hosts now redirect to .moe. POSTs must start at the canonical
  // origin because the host correctly rejects cross-origin POST redirects.
  const mirrors = ['kagane.moe'];
  const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
  const sorts = ['', 'total_views','avg_views','avg_views_today','avg_views_week','avg_views_month','updated_at','current_books','created_at'];
  const sortLabels = ['Relevance','Popular (total)','Popular (average)','Popular (today)','Popular (week)','Popular (month)','Latest','Chapter count','Created'];
  let settings, base, api;
  function fail(code, message) { const error = new Error(message); error.code = code; throw error; }
  function expect(ok, message) { if (!ok) fail('ParseChanged', message); }
  function array(value, name) { expect(Array.isArray(value), name + ' missing or changed'); return value; }
  function text(value, name) { expect(typeof value === 'string' && value.trim(), name + ' missing'); return value.trim(); }
  function id(value) { const out = text(value, 'ID'); expect(uuid.test(out), 'Kagane ID changed'); return out; }
  function seriesId(value) {
    const raw = text(value, 'Series ID');
    if (uuid.test(raw)) return raw;
    const match = raw.match(/(?:^|\/series\/)(?:[a-z0-9_-]+-)?([0-9a-f-]{36})(?:\/?$)/i);
    expect(match && uuid.test(match[1]), 'Invalid Kagane series identity'); return match[1];
  }
  function request(path, body, extra) {
    // Current upstream sends every API call as Fetch.XHR; its image-host note
    // documents HTTP 404 when the site's Origin is missing. Keep the same
    // request origin throughout metadata, integrity and page-descriptor flows.
    const headers = Object.assign({Accept:'application/json', Referer:base + '/', Origin:base}, extra || {});
    if (body !== undefined) headers['Content-Type'] = 'application/json';
    const r = JSON.parse(nova.http.request(JSON.stringify({url:base + path, method:body === undefined ? 'GET' : 'POST', headers, body:body === undefined ? null : typeof body === 'string' ? body : JSON.stringify(body)})));
    if (r.error) fail(r.error.code || 'Network', r.error.message || 'Kagane request failed');
    if (r.status === 429) fail('RateLimited', 'Kagane rate limit; retry later');
    if (r.status === 401) fail('AuthenticationRequired', 'Kagane authentication required');
    if (r.status === 403 || r.status === 507) fail('ChallengeRequired', 'Kagane requires a fresh session; reopen the chapter');
    if (r.status === 404) {
      // These are collection/session endpoints, not missing series/chapter IDs.
      if (/^\/api\/(?:integrity$|v2\/(?:search\/series|genres\/list|tags\/list|sources\/list)(?:\?|$))/.test(path)) fail('ParseChanged', 'Kagane API endpoint returned HTTP 404; its request requirements or endpoint may have changed');
      fail('NotFound', 'Kagane resource not found');
    }
    if (r.status < 200 || r.status >= 300) fail('Network', 'Kagane HTTP ' + r.status);
    // GET / primes the scoped cookie jar; its HTML never enters the parser.
    if (path === '/') {
      const canonical = String(r.url || '').match(/^https:\/\/(kagane\.(?:moe|cx|net))(?:\/|$)/i);
      if (canonical) { base = 'https://' + canonical[1].toLowerCase(); api = base + '/api/v2'; }
      return null;
    }
    let result; try { result = JSON.parse(r.body); } catch (_) { fail('ParseChanged', 'Kagane returned non-JSON markup'); }
    expect(result && typeof result === 'object' && !result.error, 'Kagane API response changed');
    return result;
  }
  function get(path) { return request('/api/v2' + path); }
  function post(path, body) { return request('/api/v2' + path, body); }
  function cleanTitle(value) {
    let name = text(value,'Title');
    if (settings.cleanTitle) name = name.replace(/(\([^()]*\)|\[[^\[\]]*\])\s*$/, '').trim();
    return name || text(value, 'Title');
  }
  function series(row, detailed, identity) {
    const sid = id(identity || row.series_id);
    let title = cleanTitle(row.title);
    if (!settings.cleanTitle && settings.showEdition && row.edition_info) title += ' (' + row.edition_info + ')';
    const cover = detailed ? (row.series_covers || [])[0]?.image_id : row.cover_image_id;
    const out = {id:sid,title,url:base + '/series/' + sid,coverUrl:cover ? api + '/image/' + id(cover) : null};
    if (!detailed) return out;
    const staff = array(row.series_staff || [],'Staff').slice().sort((a,b)=>(a.priority || 0)-(b.priority || 0));
    out.authors = [...new Set(staff.filter(x=>['author','story','original creator','artist','art'].includes(String(x.role).toLowerCase().trim())).map(x=>text(x.name,'Staff name')))];
    out.genres = [row.format, ...array(row.genres || [],'Genres').map(x=>text(x.genre_name,'Genre'))].filter(Boolean);
    out.status = ({Ongoing:'ongoing',Completed:'completed',Hiatus:'on_hiatus',Abandoned:'cancelled'})[row.upload_status] || 'unknown';
    let description = row.description || '';
    if (description) {
      const rows = JSON.parse(nova.html.select('<div>' + description.replace(/\n/g,'<br>') + '</div>', 'div', JSON.stringify({value:{value:'text'}})));
      description = rows[0]?.value || '';
    }
    const extra = [];
    if (settings.showDetails) {
      if (row.format) extra.push('Format: ' + row.format);
      if (row.age_rating) extra.push('Content rating: ' + row.age_rating);
      if (row.start_year) extra.push('Year: ' + row.start_year + (row.end_year > row.start_year ? '–' + row.end_year : ''));
      if (row.original_language) extra.push('Original language: ' + row.original_language);
      if (row.translated_language) extra.push('Translated language: ' + row.translated_language);
      if (row.average_rating != null) extra.push('Rating: ' + (row.average_rating / 10).toFixed(1) + '/10');
    }
    const alternate = array(row.series_alternate_titles || [],'Alternate titles').map(x=>text(x.title,'Alternate title'));
    if (alternate.length) extra.push('Associated names: ' + alternate.join(' · '));
    out.description = [description,extra.join('\n')].filter(Boolean).join('\n\n') || null;
    return out;
  }
  function choices(values) { return values.map(value=>({label:value,value})); }
  function metadata() {
    return {genres:array(get('/genres/list'),'Genres'), tags:array(get('/tags/list'),'Tags'), sources:array(post('/sources/list',{source_types:null}).sources,'Sources')};
  }
  function filters() {
    const meta = metadata();
    const options = (rows, key, name) => rows.map(x=>({value:id(x[key]),label:text(x[name],name)})).sort((a,b)=>a.label.localeCompare(b.label));
    return {notice:'Safe content by default. Prefix tag, person or group names with - to exclude. Year/chapter count accepts 2024, 2024-2026 or 2024+.',filters:[
      {id:'sort',name:'Sort by',type:'select',options:sorts.map((value,i)=>({value,label:sortLabels[i]}))},
      {id:'ascending',name:'Ascending order',type:'bool',default:false},
      {id:'formats',name:'Formats',type:'multiselect',options:choices(['Manga','Manhwa','Manhua','Comic','Other'])},
      {id:'statuses',name:'Publication status',type:'multiselect',options:choices(['Ongoing','Completed','Hiatus','Abandoned'])},
      {id:'genres',name:'Genres',type:'tristate',options:options(meta.genres,'id','genre_name')},
      {id:'matchAllGenres',name:'Match all selected genres',type:'bool',default:true},
      {id:'sources',name:'Sources',type:'multiselect',options:options(meta.sources.filter(x=>settings.sourceDisplay !== 'official' || x.source_type === 'Official'),'source_id','title')},
      {id:'tags',name:'Tags (comma-separated)',type:'text'},{id:'matchAllTags',name:'Match all selected tags',type:'bool',default:true},
      {id:'year',name:'Year',type:'text'},{id:'chapterCount',name:'Chapter count',type:'text'},{id:'person',name:'Person (comma-separated)',type:'text'},{id:'group',name:'Group (comma-separated)',type:'text'}
    ]};
  }
  function range(raw, name) {
    if (!raw) return null;
    const match = String(raw).trim().match(/^(\d+)(?:\s*(-)\s*(\d+)|(\+))?$/);
    expect(match, 'Invalid ' + name + ' range');
    const min = Number(match[1]), max = match[4] ? null : match[3] ? Number(match[3]) : min;
    expect(Number.isSafeInteger(min) && (max == null || Number.isSafeInteger(max) && max >= min),'Invalid ' + name + ' range');
    return max == null ? {min} : {min,max};
  }
  function names(raw, resolve) {
    const include = [], exclude = [];
    for (const name of String(raw || '').split(',').map(x=>x.trim()).filter(Boolean)) {
      const minus = name.startsWith('-');
      const value = minus ? name.slice(1).trim() : name;
      if (value) (minus ? exclude : include).push(...resolve(value));
    }
    return {values:[...new Set(include)],exclude:[...new Set(exclude)]};
  }
  function search(r, language) {
    const f = r.filters || {}, ratings = ['Safe','FifteenPlus','EighteenPlus'];
    const maxRating = ['safe','fifteenplus','eighteenplus'].indexOf(settings.contentRating || 'safe');
    const body = {source_type:settings.sourceDisplay === 'official' ? ['Official'] : ['Official','Unofficial','Mixed'],content_lang:[language],age_rating:ratings.slice(0,maxRating + 1)};
    if (maxRating < 2 || !settings.showPornographic) body.is_pornographic = false;
    if (maxRating < 2 || !settings.showDisturbing) body.is_disturbing = false;
    if (r.query?.trim()) body.title = r.query.trim();
    for (const [field, target, allowed] of [['formats','format',['Manga','Manhwa','Manhua','Comic','Other']], ['statuses','upload_status',['Ongoing','Completed','Hiatus','Abandoned']]]) {
      if (f[field]?.length) { expect(f[field].every(x=>allowed.includes(x)), 'Invalid ' + field); body[target] = f[field]; }
    }
    if (f.sources?.length) { expect(f.sources.length <= 50,'Select at most 50 sources'); body.source_id = f.sources.map(id); }
    if (f.genres) {
      const included = array(f.genres.include || [],'Included genres').map(id), excluded = array(f.genres.exclude || [],'Excluded genres').map(id);
      if (included.length || excluded.length) body.genres = {values:included,exclude:excluded,match_all:f.matchAllGenres !== false};
    }
    if (f.tags) {
      const data = array(get('/tags/list'),'Tags');
      body.tags = names(f.tags,name=>{const found = data.find(x=>String(x.tag_name).toLowerCase() === name.toLowerCase());expect(found,'Unknown tag: ' + name);return [id(found.id)];});
      body.tags.match_all = f.matchAllTags !== false;
    }
    if (f.person) body.staff = names(f.person,name=>array(post('/search/staff',{name}).content,'Staff').filter(x=>[x.name,x.native_name,...(x.alternate_names || [])].some(n=>String(n || '').toLowerCase()===name.toLowerCase())).map(x=>id(x.staff_id)));
    if (f.group) body.groups = names(f.group,name=>array(get('/groups/search?q='+encodeURIComponent(name)+'&limit=8').items,'Groups').filter(x=>String(x.title).toLowerCase()===name.toLowerCase()).map(x=>id(x.group_id)));
    const year = range(f.year,'year'), count = range(f.chapterCount,'chapter count');
    if (year) body.start_year = year;
    if (count) body.current_books = count;
    const page = r.page == null ? 1 : Number(r.page); expect(Number.isSafeInteger(page) && page >= 1,'Invalid page');
    let sort = (r.catalogue || r.kind) === 'latest' ? 'updated_at' : r.op === 'browse' ? 'total_views' : '';
    if (f.sort) {expect(sorts.includes(f.sort),'Unknown sort');sort=f.sort;}
    const query = '?page='+(page-1)+'&size=35'+(sort ? '&sort='+encodeURIComponent(sort+(f.ascending ? ',asc' : ',desc')) : '');
    const result = post('/search/series'+query, body);
    expect(typeof result.last === 'boolean','Pagination changed');
    return {items:array(result.content,'Search results').map(x=>series(x,false)),hasNextPage:!result.last};
  }
  function chapters(r, language) {
    const sid = seriesId(r.seriesId || r.id), details = get('/series/'+sid);
    const result = get('/series/'+sid+'/books/index?fields=List');
    const fields = array(result.fields,'Chapter fields'), rows = array(result.books,'Chapter rows'), groups = array(result.groups,'Chapter groups');
    expect(rows.length === result.count, 'Incomplete Kagane chapter index');
    const required = ['book_id','sort_no','chapter_no','volume_no','group_ix','released_at','title','page_count'];
    expect(new Set(fields).size === fields.length && required.every(x=>fields.includes(x)), 'Chapter fields changed');
    const books = rows.map(row=>{expect(Array.isArray(row) && row.length === fields.length,'Chapter row changed');return Object.fromEntries(fields.map((key,i)=>[key,row[i]]));});
    books.forEach(b=>expect(typeof b.sort_no === 'number' && Number.isFinite(b.sort_no),'Chapter sort number changed'));
    books.sort((a,b)=>b.sort_no-a.sort_no);
    const seen = new Set();
    return {chapters:books.map((b,i)=>{
      const bid = id(b.book_id);expect(!seen.has(bid),'Duplicate chapter ID');seen.add(bid);
      let title = typeof b.title === 'string' ? b.title.trim() : '';
      const ch = b.chapter_no == null ? '' : String(b.chapter_no), vol = b.volume_no == null ? '' : String(b.volume_no);
      const prefix = [(vol ? 'Vol.'+vol : ''),(ch ? 'Ch.'+ch : '')].filter(Boolean).join(' ');
      const mode = settings.chapterTitle || 'optional';
      if (mode === 'vol_local') title = prefix || title;
      else if (mode === 'vol_chapter') title = [prefix,title].filter(Boolean).join(' ');
      else if (mode === 'always') title = [ch ? 'Ch.'+ch : prefix,title].filter(Boolean).join(' ');
      else if (!title) title = ch ? 'Ch.'+ch : prefix;
      const special = ['Dark Horse Comics','Flame Comics','MangaDex','Square Enix Manga'].includes(details.format);
      const number = special ? String(b.sort_no) : /^\d+(?:\.\d+)?$/.test(ch) ? ch : null;
      const out = {id:bid,seriesId:sid,title:title || 'Chapter '+(number || i+1),number,sourceOrder:i,url:base+'/series/'+sid+'/reader/'+bid,language,scanlator:array(b.group_ix,'Group indices').map(g=>{expect(Number.isInteger(g) && groups[g],'Chapter group changed');return text(groups[g].title,'Group');}).join(', ')};
      if (b.released_at != null) {expect(Number.isSafeInteger(b.released_at),'Chapter date changed');const date=new Date(b.released_at*1000);expect(Number.isFinite(date.getTime()),'Chapter date invalid');out.uploadedAt=date.toISOString();}
      return out;
    })};
  }
  function pages(r) {
    let book = r.chapterId || r.id;
    if (r.op === 'resolvePage') {
      const match = text(r.url,'Page URL').match(/^https:\/\/(?:[a-z0-9-]+\.)*(?:kstatic\.to|kagane\.(?:moe|cx|net))\/api\/v2\/books\/page\/(?:datasaver\/)?([0-9a-f-]{36})\/([0-9a-f-]{36})\.[a-z0-9]+(?:\?.*)?$/i);
      expect(match,'Invalid Kagane page refresh URL');book=match[1];r.pageId=match[2];
    } else if (typeof book === 'string' && book.includes('/')) book=book.split('/').pop();
    book = id(book);
    request('/');
    const integrity = request('/api/integrity','');
    const token = text(integrity.token,'Integrity token');
    const result = postWithIntegrity('/books/'+book+'?is_datasaver='+!!settings.dataSaver, token);
    const cache = text(result.cache_url,'Page cache URL').replace(/\/$/,'');
    expect(/^https:\/\/(?:[a-z0-9-]+\.)*(?:kstatic\.to|kagane\.(?:moe|cx|net))$/i.test(cache),'Unrecognized Kagane cache host; addon permission update required');
    const access = text(result.access_token,'Page access token');
    const source = array(result.manifest?.pages,'Page manifest');expect(source.length > 0,'Empty Kagane chapter manifest');
    const seen = new Set(), ordinals = new Set();
    const pages = source.map(p=>{
      const pid = id(p.page_id), ext = p.ext || 'jxl';
      expect(['jxl','jpg','jpeg','png','webp','avif'].includes(ext),'Unsupported Kagane image format');
      expect(Number.isInteger(p.page_no) && p.page_no >= 0 && !ordinals.has(p.page_no) && !seen.has(pid),'Duplicate or invalid page identity');ordinals.add(p.page_no);seen.add(pid);
      return {id:pid,ordinal:p.page_no,url:cache+'/api/v2/books/page/'+(settings.dataSaver ? 'datasaver/' : '')+book+'/'+pid+'.'+ext+'?token='+encodeURIComponent(access),headers:{Referer:base+'/',Origin:base}};
    }).sort((a,b)=>a.ordinal-b.ordinal);
    if (r.op === 'resolvePage') {const page=pages.find(p=>p.id === r.pageId);expect(page,'Page disappeared from refreshed manifest');return {pages:[page]};}
    return {pages};
  }
  function postWithIntegrity(path, token) { return request('/api/v2'+path,{}, {'x-integrity-token':token}); }
  globalThis.novaProvider = {handle(r) {
    try {
      settings = r.settings || {};
      const host = settings.mirror || 'kagane.moe';expect(mirrors.includes(host),'Unknown mirror');base='https://'+host;api=base+'/api/v2';
      if (r.op === 'capabilities') return {sources:languages.map(language=>({id:'kagane.'+language,name:'Kagane',language,nsfw:true,operations}))};
      const language = (r.sourceId || 'kagane.en').replace(/^kagane\./,'');expect(languages.includes(language),'Unknown Kagane language');
      if (r.op === 'browse' || r.op === 'search') return search(r,language);
      if (r.op === 'filters') return filters();
      if (r.op === 'details') {const sid=seriesId(r.seriesId || r.id);return series(get('/series/'+sid),true,sid);}
      if (r.op === 'chapters') return chapters(r,language);
      if (r.op === 'pages' || r.op === 'resolvePage') return pages(r);
      fail('ParseChanged','Unsupported Kagane operation');
    } catch(e) { return {error:{code:e.code || 'ParseChanged',message:e.message || String(e)}}; }
  }};
})();
