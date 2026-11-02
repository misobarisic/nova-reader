/* Apache-2.0; modified JavaScript port of pinned Keiyoushi sources. See NOTICE.md. */
(function () {
  'use strict';
  const id = 'cosplaytele', name = 'CosplayTele', base = 'https://cosplaytele.com';
  const hosts = ['cosplaytele.com', 'www.cosplaytele.com'];
  const operations = ['capabilities', 'browse', 'search', 'filters', 'details', 'chapters', 'pages'];
  function fail(code, message) { const error = new Error(message); error.sourceCode = code; throw error; }
  function select(html, css, fields) { return JSON.parse(nova.html.select(html, css, JSON.stringify(fields))); }
  function one(html, css, value = 'text') { return select(html, css, {value: {value}})[0]?.value || ''; }
  function absolute(value, context = base + '/') { return nova.url.resolve(value, context); }
  function checkedUrl(value, context = base + '/') {
    const full = absolute(value, context);
    const match = /^https:\/\/([^/:?#]+)(\/[^?#]*)?(?:[?#].*)?$/.exec(full);
    if (!match || !hosts.includes(match[1])) fail('ParseChanged', 'Source URL must use HTTPS on a declared source host');
    return full;
  }
  function canonical(value, context = base + '/') {
    if (typeof value !== 'string' || !value.trim()) fail('NotFound', 'Missing gallery identity');
    const full = checkedUrl(value, context);
    const path = full.replace(/^https:\/\/[^/]+/, '').split(/[?#]/)[0].replace(/\/+$/, '') || '/';
    if (!isPost(path)) fail('ParseChanged', 'Expected a gallery post URL');
    return path;
  }
  function fetch(url) {
    const response = JSON.parse(nova.http.get(checkedUrl(url), JSON.stringify({Referer: base + '/', Origin: base})));
    if (response.error) fail(response.error.code || 'Network', response.error.message || 'Source request failed');
    if (response.status === 401) fail('AuthenticationRequired', 'Source authentication required');
    if (response.status === 429) fail('RateLimited', 'Source rate limit; retry later');
    if (response.status === 403 || (/cf-chl-|challenge-platform/i.test(response.body || '') || /<(?:title|h1)[^>]*>\s*(?:just a moment|verify you are human)[.!…\s]*</i.test(response.body || '') || /^\s*<html>\s*Just a moment\s*<\/html>\s*$/i.test(response.body || ''))) fail('ChallengeRequired', 'Source requires an interactive challenge; browser handoff is unavailable');
    if (response.status === 404) fail('NotFound', 'Gallery resource not found');
    if (response.status < 200 || response.status >= 300) fail('Network', 'Source HTTP ' + response.status);
    if (typeof response.body !== 'string') fail('ParseChanged', 'Source response body missing');
    return {html: response.body, url: checkedUrl(response.url || url)};
  }
  function image(value, doc) { return value.trim() ? checkedUrl(value.trim(), doc.url) : null; }
  function unique(values) { return [...new Set(values.filter(Boolean))]; }
  function requireHeading(doc) {
    const title = one(doc.html, HEADING);
    if (!title) fail('ParseChanged', 'Gallery title missing');
    return title;
  }
  function videoOnly(doc, title) {
    return /\(video\)\s*$/i.test(title) || select(doc.html, 'video, iframe[src*="video"], .video-player', {value: {value: 'html'}}).length > 0;
  }
  function listing(doc) {
    const rows = select(doc.html, LISTING, {html: {value: 'html'}});
    if (!rows.length && !select(doc.html, '.no-results, .not-found, .nothing-found', {value: {value: 'html'}}).length) fail('ParseChanged', 'Gallery listing markup missing');
    return {items: rows.map(row => listItem(row.html, doc)), hasNextPage: select(doc.html, NEXT, {value: {value: 'href'}}).length > 0};
  }
  function details(doc, identity = canonical(doc.url)) {
    const title = requireHeading(doc), galleryId = identity;
    return {id: galleryId, title, url: base + galleryId, coverUrl: detailCover(doc), description: DESCRIPTION(title), authors: [], genres: genres(doc), status: 'completed'};
  }
  function gallery(r, op) {
    const requested = canonical(r.url || (op === 'pages' ? r.chapterId : r.seriesId) || r.id);
    const doc = fetch(base + requested), title = requireHeading(doc), galleryId = requested;
    if (op === 'details') return details(doc, requested);
    if (op === 'chapters') {
      const date = one(doc.html, DATE, 'datetime');
      return {chapters: [{id: galleryId, seriesId: galleryId, title: 'Gallery', number: '1', sourceOrder: 0, url: base + galleryId, language: 'all', uploadedAt: /^\d{4}-\d{2}-\d{2}(?:T.*)?$/.test(date) ? date.slice(0, 10) : null}]};
    }
    const urls = pageImages(doc);
    if (!urls.length) {
      if (videoOnly(doc, title)) fail('ParseChanged', 'Video-only post: Nova Reader can display image galleries only');
      fail('ParseChanged', 'Gallery image markup missing');
    }
    return {pages: urls.map((url, index) => ({id: String(index), url, headers: {Referer: doc.url, Origin: base}}))};
  }
  const HEADING = '.entry-title', LISTING = 'main div.box', NEXT = '.next.page-number', DATE = 'time.updated';
  const DESCRIPTION = title => title;
  const categories = [['All', ''], ['Cosplay Nude', 'category/nude'], ['Cosplay Ero', 'category/no-nude'], ['Cosplay', 'category/cosplay']];
  function isPost(path) { const segments = path.split('/').filter(Boolean); return segments.length > 0 && !['category', 'tag', 'page', 'explore-categories', 'wp-json'].includes(segments[0]); }
  function listItem(html, doc) {
    const href = one(html, 'h5 a', 'href'), title = one(html, 'h5 a');
    if (!href || !title) fail('ParseChanged', 'Gallery listing title/link missing');
    const galleryId = canonical(href, doc.url);
    return {id: galleryId, title, url: base + galleryId, coverUrl: image(one(html, 'img', 'src'), doc)};
  }
  function detailCover() { return null; }
  function tags(doc) {
    return select(doc.html, '#main a[href]', {label: {value: 'text'}, href: {value: 'href'}}).map(row => {
      const full = absolute(row.href, doc.url);
      if (!/^https:\/\/(?:www\.)?cosplaytele\.com\/(?:tag|category)\/[^?#]+/.test(full)) return null;
      return {label: row.label, value: full.replace(/^https:\/\/[^/]+\//, '').split(/[?#]/)[0].replace(/\/+$/, '')};
    }).filter(row => row && row.label);
  }
  function genres(doc) { return unique(tags(doc).map(row => row.label)); }
  function pageImages(doc) {
    return select(doc.html, '.gallery-item img', {src: {value: 'src'}}).map(row => {
      if (!row.src.trim()) fail('ParseChanged', 'Gallery image URL missing');
      return checkedUrl(row.src.trim(), doc.url);
    });
  }
  function popular(page) {
    // Upstream starts at page * 20. Reader page 1 must include the first batch.
    const doc = fetch(base + '/wp-json/wordpress-popular-posts/v1/popular-posts?offset=' + ((page - 1) * 20) + '&limit=20&range=last7days&embed=true&_embed=wp%3Afeaturedmedia&_fields=title%2Clink%2C_embedded%2C_links.wp%3Afeaturedmedia');
    let rows;
    try { rows = JSON.parse(doc.html); } catch (_) { fail('ParseChanged', 'Popular-post JSON is invalid'); }
    if (!Array.isArray(rows)) fail('ParseChanged', 'Popular-post JSON must be an array');
    return {items: rows.map(row => {
      if (!row || typeof row.title?.rendered !== 'string' || !row.title.rendered.trim() || typeof row.link !== 'string') fail('ParseChanged', 'Popular-post title/link missing');
      const galleryId = canonical(row.link, doc.url), title = one('<div>' + row.title.rendered + '</div>', 'div');
      if (!title) fail('ParseChanged', 'Popular-post title missing');
      const media = row._embedded?.['wp:featuredmedia'];
      if (media != null && !Array.isArray(media)) fail('ParseChanged', 'Popular-post media must be an array');
      const cover = media?.[0]?.source_url;
      if (cover != null && typeof cover !== 'string') fail('ParseChanged', 'Popular-post cover URL invalid');
      return {id: galleryId, title, url: base + galleryId, coverUrl: cover ? checkedUrl(cover, doc.url) : null};
    }), hasNextPage: rows.length >= 20};
  }
  function dispatch(r) {
    const op = r.op || r.operation;
    if (r.sourceId && r.sourceId !== id) fail('NotFound', 'Unknown source');
    if (op === 'capabilities') return {sources: [{id, name, language: 'all', nsfw: true, operations, catalogues: ['popular', 'latest']}]};
    if (op === 'filters') {
      const found = tags(fetch(base + '/explore-categories/'));
      if (!found.length) fail('ParseChanged', 'Category discovery links missing');
      const options = categories.map(row => ({label: row[0], value: row[1]}));
      for (const row of found) { const old = options.findIndex(option => option.label === row.label); if (old >= 0) options[old] = row; else if (!options.some(option => option.value === row.value)) options.push(row); }
      return {notice: 'One category or tag filter may be combined with text search.', filters: [{id: 'category', type: 'select', name: 'Category', options}]};
    }
    if (op === 'browse' || op === 'search') {
      const page = Math.max(1, Math.floor(Number(r.page) || 1));
      if (op === 'browse' && (r.catalogue || r.kind || 'popular') === 'popular') return popular(page);
      if (op === 'browse' && (r.catalogue || r.kind) !== 'latest') fail('NotFound', 'Unknown catalogue');
      const category = op === 'search' ? String(r.filters?.category || '') : '';
      if (category && !/^(category|tag)\/[a-zA-Z0-9_%.-]+(?:\/[a-zA-Z0-9_%.-]+)*$/.test(category)) fail('ParseChanged', 'Invalid category filter');
      if (category.split('/').some(segment => segment === '.' || segment === '..')) fail('ParseChanged', 'Invalid category filter');
      let url = base + (category ? '/' + category + '/page/' + page : '/page/' + page + '/');
      if (op === 'search' && r.query) url += '?s=' + encodeURIComponent(String(r.query));
      return listing(fetch(url));
    }
    if (['details', 'chapters', 'pages'].includes(op)) return gallery(r, op);
    fail('NotFound', 'Unsupported operation ' + op);
  }
  globalThis.novaProvider = {handle(request) { try { return dispatch(request); } catch (error) { if (!error.sourceCode) throw error; return {error: {code: error.sourceCode, message: error.message}}; } }};
})();
