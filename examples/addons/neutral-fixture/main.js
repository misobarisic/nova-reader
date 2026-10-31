/* Newly authored neutral fixtures. CC0-1.0; see NOTICE.md. */
(function () {
  const series = {id: '/fixture/landscapes', title: 'Neutral shapes and colors', url: '',
    coverUrl: 'addon://neutral-fixture/pages/portrait.png',
    description: 'Generated geometric fixtures for reader testing. No network access.', authors: [], genres: ['Fixtures'], status: 'completed'};
  const kinds = ['portrait', 'wide', 'tall', 'mixed', 'hundreds'];
  const sizes = {portrait: [600,900], wide: [1800,600], tall: [128,12000]};
  const chapters = kinds.map((kind, sourceOrder) => ({id: '/fixture/' + kind,
    seriesId: series.id, title: kind === 'hundreds' ? '320 pages' : kind,
    number: String(sourceOrder + 1), sourceOrder, url: '', language: 'en'}));
  globalThis.novaProvider = {handle(r) {
    const op = r.op || r.operation;
    if (op === 'capabilities') return {sources:[{id:'neutral-fixture',name:'Neutral reader fixtures',language:'en',nsfw:false,operations:['browse','search','filters','details','chapters','pages']}]};
    if (op === 'browse' || op === 'search') return {items: Number(r.page || 1) > 1 || (r.query && !series.title.toLowerCase().includes(r.query.toLowerCase())) ? [] : [series], hasNextPage:false};
    if (op === 'filters') return {filters:[]};
    if (op === 'details') return series;
    if (op === 'chapters') return {chapters};
    if (op === 'pages') {
      const id = r.chapterId || r.id || '/fixture/mixed';
      const kind = id.split('/').pop();
      if (!kinds.includes(kind)) return {error:{code:'NotFound',message:'Unknown neutral chapter'}};
      const count = kind === 'hundreds' ? 320 : kind === 'mixed' ? 6 : 1;
      return {pages: Array.from({length:count}, (_,index) => {
        const shape = kind === 'mixed' || kind === 'hundreds' ? ['portrait','wide','tall'][index % 3] : kind;
        return {id:String(index),url:'addon://neutral-fixture/pages/' + shape + '.png', headers:{},width:sizes[shape][0],height:sizes[shape][1]};
      })};
    }
    return {error:{code:'NotFound',message:'Unsupported fixture operation'}};
  }};
})();
