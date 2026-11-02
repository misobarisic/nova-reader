/* Apache-2.0; modified port of pinned Keiyoushi Toonily/Madara. See NOTICE.md. */
(function () {
  'use strict';
  const base = 'https://toonily.com';
  const operations = ['capabilities', 'browse', 'search', 'filters', 'details', 'chapters', 'pages'];
  const sorts = [['Relevance', ''], ['Latest', 'latest'], ['A-Z', 'alphabet'], ['Rating', 'rating'], ['Trending', 'trending'], ['Most viewed', 'views'], ['New', 'new-manga']];
  const statuses = [['Completed', 'end'], ['Ongoing', 'on-going'], ['Cancelled', 'canceled'], ['On hold', 'on-hold']];
  const lazyAttributes = ['data-src', 'data-lazy-src', 'data-lzl-src', 'data-cfsrc', 'data-manga-src'];
  function fail(code, message) { const e = new Error(message); e.sourceCode = code; throw e; }
  function select(html, css, fields) { return JSON.parse(nova.html.select(html, css, JSON.stringify(fields))); }
  function one(html, css, value = 'text') { return select(html, css, {v: {value}})[0]?.v || ''; }
  function exists(html, css) { return select(html, css, {v: {value: 'text'}}).length > 0; }
  function absolute(value, context = base + '/') { return nova.url.resolve(value, context); }
  function canonical(value, chapter) {
    if (typeof value !== 'string' || !value) fail('ParseChanged', 'Missing source identity');
    const url = absolute(value);
    // Keep identities independent of redirects, timestamps and signed image URLs.
    const match = /^https:\/\/toonily\.com\/(?:serie|webtoon)\/([^/?#]+)(?:\/([^/?#]+))?\/?(?:[?#].*)?$/.exec(url);
    if (!match || (chapter ? !match[2] : !!match[2])) fail('ParseChanged', 'Unexpected Toonily URL');
    return '/serie/' + match[1] + (chapter ? '/' + match[2] : '');
  }
  function settingCookie(request) {
    const mature = request.settings?.showMature === true;
    if (!nova.cookies?.set) fail('ParseChanged', 'Toonily requires Nova Reader 0.1.26 or newer for its mature-content setting');
    const result = JSON.parse(nova.cookies.set(base, 'toonily-mature', mature ? '1' : '0'));
    if (result.error) fail(result.error.code || 'Network', result.error.message || 'Mature-content cookie could not be set');
    if (result.ok !== true) fail('ParseChanged', 'Mature-content cookie was not applied');
  }
  function fetch(url, post = false, referer = base + '/') {
    const headers = {Referer: referer};
    if (post) { headers['Content-Type'] = 'application/x-www-form-urlencoded'; headers['X-Requested-With'] = 'XMLHttpRequest'; }
    const response = JSON.parse(nova.http.request(JSON.stringify({url, method: post ? 'POST' : 'GET', headers, body: post ? '' : null})));
    if (response.error) fail(response.error.code || 'Network', response.error.message || 'Source request failed');
    if (response.status === 401) fail('AuthenticationRequired', 'Toonily authentication required');
    if (response.status === 429) fail('RateLimited', 'Toonily rate limit; retry later');
    if (response.status === 403 || /cf-chl-|challenge-platform|<title[^>]*>\s*(?:just a moment|attention required)|^\s*(?:<html[^>]*>)?\s*(?:just a moment|verify you are human)\s*(?:<\/html>)?\s*$/i.test(response.body)) fail('ChallengeRequired', 'Toonily requires an interactive challenge; browser handoff is unavailable');
    if (response.status === 404) fail('NotFound', 'Toonily resource not found');
    if (response.status < 200 || response.status >= 300) fail('Network', 'Toonily HTTP ' + response.status);
    return {html: response.body, url: response.url || url};
  }
  function image(html, context) {
    for (const attr of lazyAttributes) {
      const value = one(html, 'img', attr).trim();
      if (value) return absolute(value, context);
    }
    const srcset = one(html, 'img', 'srcset').trim();
    if (srcset) {
      const candidates = srcset.split(',').map(x => x.trim().split(/\s+/)).filter(x => x[0]);
      candidates.sort((a, b) => (parseFloat(b[1]) || 0) - (parseFloat(a[1]) || 0));
      if (candidates.length) return absolute(candidates[0][0], context);
    }
    const src = one(html, 'img', 'src').trim();
    return src && !/^data:|^blob:/i.test(src) ? absolute(src, context) : null;
  }
  function listing(doc) {
    const rows = select(doc.html, 'div.page-item-detail, .manga__item, .c-tabs-item__content', {html: {value: 'html'}});
    if (!rows.length && !exists(doc.html, '.no-results, .no-result, .not-found, .search-no-results, .no-posts')) fail('ParseChanged', 'Toonily listing markup missing');
    const seen = {};
    const items = rows.map(row => {
      const title = one(row.html, '.post-title a');
      const href = one(row.html, '.post-title a', 'href');
      if (!title || !href) fail('ParseChanged', 'Toonily listing item has no title/link');
      const id = canonical(absolute(href, doc.url), false);
      return {id, title, url: base + id + '/', coverUrl: image(row.html, doc.url)};
    }).filter(item => !seen[item.id] && (seen[item.id] = true));
    return {items, hasNextPage: exists(doc.html, 'div.nav-previous, a.nextpostslink, #navigation-ajax')};
  }
  function choice(value, options, label) {
    const selected = value == null ? '' : String(value);
    if (!options.some(x => x[1] === selected)) fail('ParseChanged', 'Invalid ' + label + ' filter');
    return selected;
  }
  function searchUrl(r, page) {
    const f = r.filters || {};
    const sort = choice(f.sort, sorts, 'order');
    const params = [];
    const add = (key, value) => params.push(encodeURIComponent(key) + '=' + encodeURIComponent(value));
    let path;
    // Upstream Tag takes precedence over query and every filter except Order By.
    if (f.tag) {
      if (!/^[a-zA-Z0-9_-]+$/.test(f.tag)) fail('ParseChanged', 'Invalid tag slug');
      path = '/tag/' + encodeURIComponent(f.tag) + '/';
      if (sort) add('m_orderby', sort);
    } else {
      const slug = String(r.query || '').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
      path = '/search/' + (slug ? slug + '/' : '');
      for (const key of ['author', 'artist', 'release']) {
        const value = String(f[key] || '').trim();
        if (value) add(key, value);
      }
      const status = f.status || [];
      if (!Array.isArray(status)) fail('ParseChanged', 'Invalid status filter');
      for (const value of status) { choice(value, statuses, 'status'); add('status[]', value); }
      if (sort) add('m_orderby', sort);
      const adult = choice(f.adult, [['All', ''], ['None', '0'], ['Only', '1']], 'adult');
      const condition = choice(f.genreCondition, [['Any', ''], ['All', '1']], 'genre condition');
      if (adult) add('adult', adult);
      if (condition) add('op', condition);
      const genres = f.genres || [];
      if (!Array.isArray(genres)) fail('ParseChanged', 'Invalid genres filter');
      for (const genre of genres) {
        if (!/^[a-zA-Z0-9_-]+$/.test(genre)) fail('ParseChanged', 'Invalid genre slug');
        add('genre[]', genre);
      }
    }
    if (page > 1) path += 'page/' + page + '/';
    return base + path + (params.length ? '?' + params.join('&') : '');
  }
  function labeled(doc, label, own = false) {
    const rows = select(doc, '.post-content_item', {html: {value: 'html'}});
    for (const row of rows) {
      if (one(row.html, '.summary-heading').toLowerCase().startsWith(label)) return one(row.html, '.summary-content', own ? 'ownText' : 'text');
    }
    return '';
  }
  function texts(html, css) { return select(html, css, {v: {value: 'text'}}).map(x => x.v).filter(x => x && !/updating|atualizando/i.test(x)); }
  function details(doc, id) {
    const selector = 'div.post-title h3, div.post-title h1, #manga-title > h1';
    const title = one(doc.html, selector, 'ownText') || one(doc.html, selector);
    if (!title) fail('ParseChanged', 'Toonily series heading missing');
    const summary = one(doc.html, 'div.content-area div.summary__content', 'html');
    const paragraphs = texts(summary, 'p');
    let description = paragraphs.length ? paragraphs.join('\n\n') : one('<div>' + summary + '</div>', 'div');
    const alt = labeled(doc.html, 'alt', true);
    if (alt && !/updating|atualizando/i.test(alt)) description = [description, 'Alternative names: ' + alt].filter(Boolean).join('\n\n');
    const statusNodes = texts(doc.html, 'div.summary-content');
    const statusText = (labeled(doc.html, 'status') || statusNodes[statusNodes.length - 1] || '').toLowerCase();
    const status = /completed|finished/.test(statusText) ? 'completed' : /on.?going|releasing/.test(statusText) ? 'ongoing' : /on.?hold|hiatus/.test(statusText) ? 'on_hiatus' : /cancelled|canceled/.test(statusText) ? 'cancelled' : 'unknown';
    const genres = [...texts(doc.html, 'div.genres-content a, div.tags-content a'), labeled(doc.html, 'type')].filter(Boolean);
    const seen = {};
    return {id, title, url: base + id + '/', coverUrl: image(one(doc.html, 'div.summary_image', 'html'), doc.url),
      description: description || null, authors: texts(doc.html, 'div.author-content > a, div.manga-authors > a, div.artist-content > a'),
      genres: genres.filter(value => !seen[value.toLowerCase()] && (seen[value.toLowerCase()] = true)), status};
  }
  function date(value) {
    const m = /^([a-z]{3})\s+(\d{1,2}),\s*(\d{2}|\d{4})$/i.exec(value.trim());
    if (m) {
      const month = ['jan', 'feb', 'mar', 'apr', 'may', 'jun', 'jul', 'aug', 'sep', 'oct', 'nov', 'dec'].indexOf(m[1].toLowerCase());
      if (month >= 0) return (m[3].length === 2 ? '20' : '') + m[3] + '-' + String(month + 1).padStart(2, '0') + '-' + m[2].padStart(2, '0');
    }
    const now = new Date();
    if (/^today/i.test(value)) return now.toISOString().slice(0, 10);
    if (/^yesterday/i.test(value)) return new Date(now.getTime() - 86400000).toISOString().slice(0, 10);
    const relative = /^(\d+)\s+(second|minute|hour|day|week|month|year)s?\s+ago$/i.exec(value.trim());
    if (relative) {
      const n = Number(relative[1]); const unit = relative[2].toLowerCase();
      if (unit === 'month') now.setUTCMonth(now.getUTCMonth() - n);
      else if (unit === 'year') now.setUTCFullYear(now.getUTCFullYear() - n);
      else now.setTime(now.getTime() - n * ({second: 1000, minute: 60000, hour: 3600000, day: 86400000, week: 604800000}[unit]));
      return now.toISOString();
    }
    return null;
  }
  function chapters(doc, id) {
    const rows = select(doc.html, 'li.wp-manga-chapter', {html: {value: 'html'}});
    if (!rows.length && !exists(doc.html, 'ul.main, ul.version-chap, ul.row-content-chapter, .no-chapters')) fail('ParseChanged', 'Toonily chapter list missing');
    const seen = {};
    return {chapters: rows.map(row => {
      const title = one(row.html, 'a'); const href = one(row.html, 'a', 'href');
      if (!title || !href) fail('ParseChanged', 'Toonily chapter link missing');
      const chapterId = canonical(absolute(href, base + id + '/'), true);
      if (!chapterId.startsWith(id + '/')) fail('ParseChanged', 'Chapter belongs to a different Toonily series');
      const uploaded = one(row.html, 'img:not(.thumb)', 'alt') || one(row.html, 'span a', 'title') || one(row.html, 'span.chapter-release-date');
      return {id: chapterId, seriesId: id, title, number: /\b(?:chapter|ch\.?)\s*(\d+(?:\.\d+)?)/i.exec(title)?.[1] || null,
        url: base + chapterId + '/', language: 'en', uploadedAt: date(uploaded)};
    }).filter(row => !seen[row.id] && (seen[row.id] = true)).map((row, sourceOrder) => ({...row, sourceOrder}))};
  }
  function pages(doc, id) {
    // Never return a partial gallery when the site switches to protected data.
    if (exists(doc.html, '#chapter-protector-data') || /wpmangaprotectornonce|chapter_data\s*=/.test(doc.html)) fail('ParseChanged', 'Toonily protected reader data is unsupported; refresh after an addon update');
    if (exists(doc.html, '#single-pager')) fail('ParseChanged', 'Toonily did not provide the requested complete list reader');
    const rows = select(doc.html, 'div.page-break, li.blocks-gallery-item, .reading-content .text-left:not(:has(.blocks-gallery-item))', {html: {value: 'html'}});
    if (!rows.length) fail('ParseChanged', 'Toonily reader images missing');
    const urls = rows.map(row => {
      // A container with several panels would silently drop pages in a first-img parser.
      if (select(row.html, 'img', {v: {value: 'src'}}).length !== 1) fail('ParseChanged', 'Toonily reader panel structure changed');
      const url = image(row.html, doc.url);
      if (!url) fail('ParseChanged', 'Toonily reader image URL missing');
      return url;
    });
    return {pages: urls.map((url, index) => ({id: id + '/page/' + index, url, headers: {Referer: doc.url}}))};
  }
  function filters() {
    const doc = fetch(base + '/serie/');
    const genreRows = select(doc.html, 'div.genres a[href*="/genre/"]', {label: {value: 'text'}, href: {value: 'href'}});
    const seen = {};
    const genres = genreRows.map(row => ({label: row.label, value: absolute(row.href, doc.url).split(/[?#]/)[0].replace(/\/$/, '').split('/').pop()})).filter(row => row.label && row.value && !seen[row.value] && (seen[row.value] = true));
    if (!genres.length && !exists(doc.html, 'div.genres')) fail('ParseChanged', 'Toonily genre filter markup missing');
    if (genres.length > 512) fail('ParseChanged', 'Toonily genre list exceeds Reader filter limits');
    const tagsDoc = fetch(base + '/wp-manga-tag-sitemap.xml');
    const tags = select(tagsDoc.html, 'loc', {url: {value: 'text'}}).map(row => /^https:\/\/toonily\.com\/tag\/([^/?#]+)\/?$/.exec(row.url)?.[1]).filter(Boolean);
    const tagSlugs = [...new Set(tags)].sort();
    if (!tagSlugs.length && !exists(tagsDoc.html, 'urlset, sitemapindex')) fail('ParseChanged', 'Toonily tag sitemap markup missing');
    const options = rows => rows.map(([label, value]) => ({label, value}));
    return {notice: 'Tag ignores search text and all filters except Order By. Show mature content is a separate source setting.', filters: [
      ...['author', 'artist', 'release'].map(id => ({id, type: 'text', name: id === 'release' ? 'Year' : id[0].toUpperCase() + id.slice(1)})),
      {id: 'status', name: 'Status', type: 'multiselect', options: options(statuses)},
      {id: 'sort', name: 'Order By', type: 'select', options: options(sorts)},
      {id: 'adult', name: 'Adult content', type: 'select', options: options([['All', ''], ['None', '0'], ['Only', '1']])},
      {id: 'genreCondition', name: 'Match genres', type: 'select', options: options([['Any', ''], ['All', '1']])},
      {id: 'genres', name: 'Genres', type: 'multiselect', options: genres},
      // Large tag directories remain usable without truncating the source list.
      tagSlugs.length <= 199 ? {id: 'tag', name: 'Tag', type: 'select', options: [{label: 'None', value: ''}, ...tagSlugs.map(value => ({value, label: value.split('-').map(x => x[0]?.toUpperCase() + x.slice(1)).join(' ')}))]} : {id: 'tag', name: 'Tag slug (from the site tag URL)', type: 'text'}
    ]};
  }
  function dispatch(r) {
    if (r.sourceId && r.sourceId !== 'toonily.en') fail('NotFound', 'Unknown Toonily source identity');
    const op = r.op || r.operation;
    if (op === 'capabilities') return {sources: [{id: 'toonily.en', name: 'Toonily', language: 'en', nsfw: true, operations}]};
    if (!operations.includes(op)) fail('NotFound', 'Unsupported Toonily operation');
    settingCookie(r);
    const page = Math.max(1, Math.floor(Number(r.page) || 1));
    if (op === 'browse') return listing(fetch(base + '/serie/' + (page > 1 ? 'page/' + page + '/' : '') + '?m_orderby=' + ((r.catalogue || r.kind) === 'latest' ? 'latest' : 'views')));
    if (op === 'search') return listing(fetch(searchUrl(r, page)));
    if (op === 'filters') return filters();
    if (op === 'details' || op === 'chapters') {
      const id = canonical(r.url || r.seriesId || r.id, false);
      return op === 'details' ? details(fetch(base + id + '/'), id) : chapters(fetch(base + id + '/ajax/chapters/', true, base + id + '/'), id);
    }
    if (op === 'pages') {
      const id = canonical(r.url || r.chapterId || r.id, true);
      let doc = fetch(base + id + '/');
      if (exists(doc.html, '#single-pager')) doc = fetch(base + id + '/?style=list');
      return pages(doc, id);
    }
  }
  globalThis.novaProvider = {handle(request) {
    try { return dispatch(request); }
    catch (e) { if (!e.sourceCode) throw e; return {error: {code: e.sourceCode, message: e.message}}; }
  }};
})();
