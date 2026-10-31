/* Apache-2.0; ported from Keiyoushi YiiMadara. See NOTICE.md. */
(function () {
  'use strict';
  const operations = ['capabilities', 'browse', 'search', 'filters', 'details', 'chapters', 'pages'];
  const sorts = [['Default', ''], ['Trending', 'trending'], ['Newest', 'latest'], ['Rating', 'rating'], ['A-Z', 'alphabet']];
  function fail(code, message) { const e = new Error(message); e.sourceCode = code; throw e; }
  function select(html, selector, fields) { return JSON.parse(nova.html.select(html, selector, JSON.stringify(fields))); }
  function one(html, selector, field = 'text') { return select(html, selector, {value: {value: field}})[0]?.value || ''; }
  function clean(value) { return value && !['Updating', 'N/A'].includes(value) ? value : null; }
  function create(config) {
    const base = config.baseUrl.replace(/\/$/, '');
    const absolute = (url, context = base + '/') => nova.url.resolve(url, context);
    function canonical(url) {
      const full = absolute(url);
      if (!full.startsWith(base + '/' + config.mangaPath + '/')) fail('ParseChanged', 'Unexpected series URL');
      const slug = full.slice((base + '/' + config.mangaPath + '/').length).split(/[/?#]/)[0];
      if (!slug) fail('ParseChanged', 'Missing series slug');
      return '/' + config.mangaPath + '/' + slug;
    }
    function fetch(url) {
      const r = JSON.parse(nova.http.get(url, JSON.stringify({Referer: base + '/', Origin: base})));
      if (r.status === 429) fail('RateLimited', 'Source rate limit; retry later');
      if (r.status === 401) fail('AuthenticationRequired', 'Source authentication required');
      if (r.status === 403 || /cf-chl-|challenge-platform|verify you are human|just a moment/i.test(r.body)) fail('ChallengeRequired', 'Source requires an interactive challenge; browser handoff is unavailable');
      if (r.status === 404) fail('NotFound', 'Source resource not found');
      if (r.status < 200 || r.status >= 300) fail('Network', 'Source HTTP ' + r.status);
      return {html: r.body, url: r.url || url};
    }
    function image(html, context) {
      const value = one(html, 'img', 'data-src').trim() || one(html, 'img', 'src').trim();
      return value ? absolute(value, context) : null;
    }
    function listing(doc, lang, rawListing) {
      const rows = select(doc.html, 'div.page-item, div.page-item-detail, div.home-item, div.manga-item', {html: {value: 'html'}});
      // An explicit empty-state is accepted; a missing listing selector is a markup failure.
      if (!rows.length && !one(doc.html, '.no-results, .no-result, .not-found')) fail('ParseChanged', 'Listing markup missing');
      const items = rows.map(row => {
        const title = one(row.html, 'h3 a'); const href = one(row.html, 'h3 a', 'href');
        if (!title || !href) fail('ParseChanged', 'Listing item has no title/link');
        const id = canonical(absolute(href, doc.url));
        return {id, title, url: base + id, coverUrl: image(row.html, doc.url)};
      }).filter(item => {
        const raw = item.id.replace(/\/$/, '').endsWith('-raw') || / raw$/i.test(item.title);
        return lang === 'all' || rawListing || (lang === 'ko' ? raw : !raw);
      });
      return {items, hasNextPage: select(doc.html, '#blog-pager li.next a', {href: {value: 'href'}}).length > 0};
    }
    function info(html, label) {
      for (const row of select(html, '.post-content_item', {html: {value: 'html'}})) {
        const heading = one(row.html, '.summary-heading, b');
        if (heading.toLowerCase().startsWith(label.toLowerCase())) {
          const content = one(row.html, '.summary-content', 'html') || row.html.replace(/<b\b[^>]*>[\s\S]*?<\/b>/i, '');
          const links = select(content, 'a', {text: {value: 'text'}}).map(x => clean(x.text)).filter(Boolean);
          return links.length ? links.join(', ') : clean(one('<div>' + content + '</div>', 'div'));
        }
      }
      for (const row of select(html, '.minfo', {label: {value: 'ownText'}, html: {value: 'html'}})) {
        if (row.label.toLowerCase().startsWith(label.toLowerCase())) return clean(one(row.html, 'i'));
      }
      return null;
    }
    function details(doc, id) {
      const title = one(doc.html, '.post-title h1', 'ownText') || one(doc.html, '.post-title h1');
      if (!title) fail('ParseChanged', 'Series heading missing');
      const synopsis = one(doc.html, '.dsct, .ss-manga, .post-content_item .entry-content', 'html');
      const paragraphs = select(synopsis, 'p', {text: {value: 'text'}}).map(x => x.text).filter(Boolean);
      const description = paragraphs.length ? paragraphs.join('\n\n') : one('<div>' + synopsis + '</div>', 'div');
      const alt = info(doc.html, 'Alternative');
      return {id, title, url: base + id, coverUrl: image(one(doc.html, '.summary_image', 'html'), doc.url),
        authors: (info(doc.html, 'Author') || '').split(', ').filter(Boolean), artist: info(doc.html, 'Artist'),
        genres: [info(doc.html, 'Genre'), info(doc.html, 'Type')].filter(Boolean).join(', ').split(', ').filter(Boolean),
        status: ['ongoing', 'completed'].includes((info(doc.html, 'Status') || '').toLowerCase()) ? info(doc.html, 'Status').toLowerCase() : 'unknown',
        description: [description, alt ? 'Alternative names: ' + alt : ''].filter(Boolean).join('\n\n') || null};
    }
    function date(value) {
      const m = /^(\d{2}) ([A-Za-z]{3}) (\d{4}|\d{2})$/.exec(value || '');
      if (!m) return null;
      const month = ['jan','feb','mar','apr','may','jun','jul','aug','sep','oct','nov','dec'].indexOf(m[2].toLowerCase()) + 1;
      const year = m[3].length === 2 ? '20' + m[3] : m[3];
      return month ? year + '-' + String(month).padStart(2, '0') + '-' + m[1] : null;
    }
    function dispatch(r) {
      const op = r.op || r.operation; const lang = (r.sourceId || config.id + '.en').split('.').pop();
      if (!['en', 'ko', 'all'].includes(lang)) fail('NotFound', 'Unknown source language');
      const directory = lang === 'ko' ? config.rawPath : config.directoryPath;
      const page = Math.max(1, Math.floor(Number(r.page) || 1));
      if (op === 'capabilities') return {sources: ['en', 'ko', 'all'].map(language => ({id: config.id + '.' + language, name: config.name, language, nsfw: true, operations}))};
      if (op === 'browse' || op === 'search') {
        const filters = r.filters || {}; let path = directory; let url;
        if (op === 'search' && (r.query || '').trim()) {
          url = base + '/search?q=' + encodeURIComponent(r.query.replace(/'/g, '’')) + '&page=' + page;
        } else {
          if (op === 'search') path = filters.genre || filters.category || directory;
          if (!/^[a-zA-Z0-9_/-]+$/.test(path) || path.includes('..')) fail('ParseChanged', 'Invalid filter path');
          url = base + '/' + path + '/' + page;
          const sort = op === 'browse' ? ((r.catalogue || r.kind || 'popular') === 'popular' ? 'trending' : '') : (filters.sort || '');
          if (!sorts.some(x => x[1] === sort)) fail('ParseChanged', 'Invalid sort');
          if (sort) url += '?orderby=' + encodeURIComponent(sort);
        }
        return listing(fetch(url), lang, path === config.rawPath && !(r.query || '').trim());
      }
      if (op === 'filters') {
        const doc = fetch(base + '/');
        const seen = {};
        const genres = select(doc.html, 'a[href*="genre/"]', {label: {value: 'text'}, href: {value: 'href'}}).map(row => {
          const value = absolute(row.href, doc.url).replace(/^https?:\/\/[^/]+\//, '').split(/[?#]/)[0].replace(/\/$/, '');
          return {label: row.label, value};
        }).filter(row => !seen[row.value] && (seen[row.value] = true)).sort((a,b) => a.label < b.label ? -1 : a.label > b.label ? 1 : 0);
        return {notice: 'Ignored with text search; Genre takes priority over Category', filters: [
          {id: 'genre', type: 'select', options: [{label: 'All', value: ''}, ...genres]},
          {id: 'category', type: 'select', options: [{label: 'All', value: ''}, {label: 'Completed', value: 'completed'}, {label: 'Raw', value: config.rawPath}]},
          {id: 'sort', type: 'select', options: sorts.map(x => ({label: x[0], value: x[1]}))}]};
      }
      if (op === 'details' || op === 'chapters') {
        const id = canonical(r.url || r.id || r.seriesId); const doc = fetch(base + id);
        if (op === 'details') return details(doc, id);
        if (!select(doc.html, 'ul.row-content-chapter', {html: {value: 'html'}}).length) fail('ParseChanged', 'Chapter list missing');
        return {chapters: select(doc.html, 'ul.row-content-chapter li', {title: {selector: 'a', value: 'text'}, href: {selector: 'a', value: 'href'}, date: {selector: '.chapter-time', value: 'text'}}).map((row, sourceOrder) => {
          if (!row.href || !row.title) fail('ParseChanged', 'Chapter link missing');
          const url = absolute(row.href, doc.url); const chapterId = url.replace(base, '').split(/[?#]/)[0];
          const number = /\bchapter\s+(\d+(?:\.\d+)?)/i.exec(row.title)?.[1] || null;
          return {id: chapterId, seriesId: id, title: row.title, number, sourceOrder, url, language: lang, uploadedAt: date(row.date)};
        })};
      }
      if (op === 'pages') {
        const url = absolute(r.url || r.chapterId || r.id); const doc = fetch(url);
        const rows = select(doc.html, '.read-content img', {lazy: {value: 'data-src'}, src: {value: 'src'}});
        if (!rows.length) fail('ParseChanged', 'Reader images missing');
        return {pages: rows.map((row, index) => {
          const imageUrl = row.lazy.trim() || row.src.trim();
          if (!imageUrl) fail('ParseChanged', 'Reader image URL missing');
          return {id: String(index), url: absolute(imageUrl, doc.url), headers: {Referer: doc.url, Origin: base}};
        })};
      }
      fail('NotFound', 'Unsupported operation ' + op);
    }
    return {handle(request) { try { return dispatch(request); } catch (e) { if (!e.sourceCode) throw e; return {error: {code: e.sourceCode, message: e.message}}; } }};
  }
  globalThis.YiiMadara = {create};
})();
