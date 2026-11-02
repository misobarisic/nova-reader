/* Apache-2.0; modified JavaScript port of pinned Keiyoushi sources. See NOTICE.md. */
(function () {
  'use strict';
  const id = 'mitaku', name = 'Mitaku', base = 'https://mitaku.net';
  const hosts = ['mitaku.net'];
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
  const HEADING = 'article h1', LISTING = 'div.article-container article', NEXT = 'div.wp-pagenavi a.page.larger', DATE = '.entry-date';
  const DESCRIPTION = () => null;
  const categories = [['Any', ''], ['Ero Cosplay', 'ero-cosplay'], ['Nude', 'nude'], ['Sexy Set', 'sexy-set'], ['Online Video', 'online-video']];
  function isPost(path) { const segments = path.split('/').filter(Boolean); return segments.length >= 2 && !['category', 'tag', 'search', 'page'].includes(segments[0]); }
  function listItem(html, doc) {
    const href = one(html, 'a', 'href'), title = one(html, 'a', 'title') || one(html, 'h1, h2, h3');
    if (!href || !title) fail('ParseChanged', 'Gallery listing title/link missing');
    const galleryId = canonical(href, doc.url);
    return {id: galleryId, title, url: base + galleryId, coverUrl: image(one(html, 'img', 'src'), doc)};
  }
  function detailCover() { return null; }
  function genres(doc) { return unique(select(doc.html, 'article span.cat-links a, article span.tag-links a', {label: {value: 'text'}}).map(row => row.label)); }
  function pageImages(doc) {
    return select(doc.html, 'a.msacwl-img-link', {full: {value: 'data-mfp-src'}, href: {value: 'href'}}).map(row => {
      const value = row.full.trim() || row.href.trim();
      if (!value) fail('ParseChanged', 'Full-size gallery image link missing');
      return checkedUrl(value, doc.url);
    });
  }
  function dispatch(r) {
    const op = r.op || r.operation;
    if (r.sourceId && r.sourceId !== id) fail('NotFound', 'Unknown source');
    if (op === 'capabilities') return {sources: [{id, name, language: 'all', nsfw: true, operations, catalogues: ['popular']}]};
    if (op === 'filters') return {notice: 'Text search takes priority over Category, then Tag. Enter one tag.', filters: [
      {id: 'category', type: 'select', name: 'Category', options: categories.map(row => ({label: row[0], value: row[1]}))},
      {id: 'tag', type: 'text', name: 'Tag'}]};
    if (op === 'browse' || op === 'search') {
      const page = Math.max(1, Math.floor(Number(r.page) || 1)), filters = r.filters || {};
      if (op === 'browse' && (r.catalogue || r.kind || 'popular') !== 'popular') fail('NotFound', 'Mitaku supports the popular catalogue only');
      const query = op === 'search' ? String(r.query || '').trim() : '';
      let path = '/category/ero-cosplay/page/' + page;
      if (query) path = '/page/' + page + '?s=' + encodeURIComponent(query);
      else if (op === 'search' && filters.category) {
        if (!categories.some(row => row[1] === filters.category)) fail('ParseChanged', 'Unknown category filter');
        path = '/category/' + filters.category + '/page/' + page;
      } else if (op === 'search' && String(filters.tag || '').trim()) {
        const tag = String(filters.tag).trim().toLowerCase().split(/ +/).filter(Boolean).join('-');
        path = '/tag/' + encodeURIComponent(tag) + '/page/' + page;
      }
      const doc = fetch(base + path);
      if (op === 'search' && isPost(doc.url.replace(/^https:\/\/[^/]+/, '').split(/[?#]/)[0])) return {items: [details(doc)], hasNextPage: false};
      return listing(doc);
    }
    if (['details', 'chapters', 'pages'].includes(op)) return gallery(r, op);
    fail('NotFound', 'Unsupported operation ' + op);
  }
  globalThis.novaProvider = {handle(request) { try { return dispatch(request); } catch (error) { if (!error.sourceCode) throw error; return {error: {code: error.sourceCode, message: error.message}}; } }};
})();
