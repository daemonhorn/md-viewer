(function(){
'use strict';
var root=document.documentElement,mode=root.dataset.mode;
function toggleRaw(){var u=new URL(location.href);u.searchParams.set('raw',mode==='raw'?'0':'1');u.hash='';location.href=u.toString();}
// TOC (rendered mode only)
var toc=document.getElementById('toc');
if(toc&&mode==='rendered'){
  var hs=document.querySelectorAll('article h1[id],article h2[id],article h3[id],article h4[id]');
  if(hs.length>2){hs.forEach(function(h){var a=document.createElement('a');a.href='#'+h.id;a.textContent=h.textContent;a.className='l'+h.tagName[1];toc.appendChild(a);});toc.hidden=false;}
}
function toggleToc(){if(toc&&toc.children.length)toc.hidden=!toc.hidden;}
// Find
var find=document.getElementById('find'),fi=find&&find.querySelector('input');
function openFind(){find.hidden=false;fi.focus();fi.select();}
function closeFind(){find.hidden=true;fi.blur();getSelection().removeAllRanges();}
function doFind(back){if(fi.value)window.find(fi.value,false,!!back,true);}
if(fi){
  fi.addEventListener('keydown',function(e){if(e.key==='Enter'){doFind(e.shiftKey);e.preventDefault();}else if(e.key==='Escape')closeFind();e.stopPropagation();});
  find.querySelector('.prev').onclick=function(){doFind(true)};
  find.querySelector('.next').onclick=function(){doFind(false)};
}
document.addEventListener('keydown',function(e){
  if((e.ctrlKey||e.metaKey)&&e.key.toLowerCase()==='f'&&find){e.preventDefault();openFind();return;}
  if(e.ctrlKey||e.metaKey||e.altKey)return;
  if(e.key==='r'){toggleRaw();}else if(e.key==='t'){toggleToc();}
  else if(e.key==='F3'||e.key==='n'){doFind(e.shiftKey);}
});
// Optional assets: TODO(bundling) the server returns 404 until mermaid/KaTeX are vendored
// into assets/vendor and wired up in src/serve.rs::bundled_asset.
function load(src,cb){var s=document.createElement('script');s.src=src;s.onload=cb;s.onerror=function(){cb(new Error('missing'))};document.head.appendChild(s);}
if(mode==='rendered'&&root.dataset.mermaid==='1'){
  load('/__mdv/vendor/mermaid.min.js',function(err){
    var blocks=document.querySelectorAll('pre[lang=mermaid] code,code.language-mermaid');
    if(err||!window.mermaid)return;
    var dark=root.dataset.theme==='dark'||(root.dataset.theme!=='light'&&matchMedia('(prefers-color-scheme:dark)').matches);
    mermaid.initialize({startOnLoad:false,theme:dark?'dark':'default',securityLevel:'strict'});
    blocks.forEach(function(c,i){var host=c.closest('pre')||c;mermaid.render('mmd'+i,c.textContent).then(function(r){var d=document.createElement('div');d.innerHTML=r.svg;host.replaceWith(d);}).catch(function(){});});
  });
}
if(mode==='rendered'&&root.dataset.math==='1'){
  load('/__mdv/vendor/katex.min.js',function(err){
    if(err||!window.katex)return;
    var l=document.createElement('link');l.rel='stylesheet';l.href='/__mdv/vendor/katex.min.css';document.head.appendChild(l);
    document.querySelectorAll('[data-math-style]').forEach(function(el){
      try{katex.render(el.textContent,el,{displayMode:el.dataset.mathStyle==='display',throwOnError:false});}catch(e){}
    });
  });
}
})();
