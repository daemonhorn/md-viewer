(function(){
'use strict';
// All UI ids/classes use the reserved `mdv-` prefix; document content may contain any id,
// so UI lookups ignore matches inside the rendered document.
var root=document.documentElement,mode=root.dataset.mode;
function safe(f){try{f();}catch(e){}}
function ui(id){
  var all=document.querySelectorAll('[id="'+id+'"]');
  for(var i=0;i<all.length;i++){if(!all[i].closest('article'))return all[i];}
  return null;
}
function toggleRaw(){var u=new URL(location.href);u.searchParams.set('raw',mode==='raw'?'0':'1');u.hash='';location.href=u.toString();}
// TOC (rendered mode only)
var toc=ui('mdv-toc'),main=ui('mdv-main');
safe(function(){
  if(!toc||!main||mode!=='rendered')return;
  var hs=main.querySelectorAll('article h1[id],article h2[id],article h3[id],article h4[id]');
  if(hs.length>2){hs.forEach(function(h){var a=document.createElement('a');a.href='#'+encodeURIComponent(h.id);a.textContent=h.textContent;a.className='mdv-l'+h.tagName[1];toc.appendChild(a);});toc.hidden=false;}
});
function toggleToc(){if(toc&&toc.children.length)toc.hidden=!toc.hidden;}
// Print: the native print dialog is opened by the Rust side (webview.print()) on an IPC message.
function isPrintKey(e){return e.ctrlKey&&!e.altKey&&!e.shiftKey&&!e.metaKey&&typeof e.key==='string'&&e.key.toLowerCase()==='p';}
function requestPrint(){
  try{if(window.ipc&&typeof window.ipc.postMessage==='function'){window.ipc.postMessage('print');return true;}}catch(e){}
  return false;
}
safe(function(){var b=ui('mdv-print');if(b)b.addEventListener('click',function(){requestPrint();});});
window.addEventListener('beforeprint',function(){safe(function(){root.classList.add('mdv-printing');if(find)find.hidden=true;try{getSelection().removeAllRanges();}catch(e){}});});
window.addEventListener('afterprint',function(){safe(function(){root.classList.remove('mdv-printing');});});
// Find
var find=ui('mdv-find'),fi=find&&find.querySelector('input');
function openFind(){if(!find||!fi)return;find.hidden=false;fi.focus();fi.select();}
function closeFind(){if(!find||!fi)return;find.hidden=true;fi.blur();getSelection().removeAllRanges();}
function doFind(back){if(fi&&fi.value)window.find(fi.value,false,!!back,true);}
safe(function(){
  if(!fi)return;
  fi.addEventListener('keydown',function(e){if(e.key==='Enter'){doFind(e.shiftKey);e.preventDefault();}else if(e.key==='Escape')closeFind();if(!isPrintKey(e))e.stopPropagation();});
  var p=find.querySelector('.mdv-prev'),n=find.querySelector('.mdv-next');
  if(p)p.onclick=function(){doFind(true)};
  if(n)n.onclick=function(){doFind(false)};
});
document.addEventListener('keydown',function(e){
  safe(function(){
    if(isPrintKey(e)){e.preventDefault();requestPrint();return;}
    var t=e.target,tag=t&&t.tagName;
    if(tag==='INPUT'||tag==='TEXTAREA'||(t&&t.isContentEditable))return;
    if((e.ctrlKey||e.metaKey)&&e.key.toLowerCase()==='f'&&find){e.preventDefault();openFind();return;}
    if(e.ctrlKey||e.metaKey||e.altKey)return;
    if(e.key==='r'){toggleRaw();}else if(e.key==='t'){toggleToc();}
    else if(e.key==='F3'||e.key==='n'){doFind(e.shiftKey);}
  });
});
// Optional assets (mermaid is embedded by default; KaTeX is not bundled yet and 404s).
function load(src,cb){var s=document.createElement('script');s.src=src;s.onload=function(){cb()};s.onerror=function(){cb(new Error('missing'))};document.head.appendChild(s);}
if(mode==='rendered'&&root.dataset.mermaid==='1'){
  load('/__mdv/vendor/mermaid.min.js',function(err){
    safe(function(){
      if(err||!window.mermaid||typeof window.mermaid.render!=='function')return;
      var blocks=document.querySelectorAll('pre[lang=mermaid] code,code.language-mermaid');
      var dark=root.dataset.theme==='dark'||(root.dataset.theme!=='light'&&matchMedia('(prefers-color-scheme:dark)').matches);
      window.mermaid.initialize({startOnLoad:false,theme:dark?'dark':'default',securityLevel:'strict'});
      // When the screen theme is dark, also render a light copy that is only shown when printing.
      var jobs=[];
      blocks.forEach(function(c,i){
        safe(function(){
          var host=c.closest('pre')||c,src=c.textContent;
          jobs.push(window.mermaid.render('mdv-mmd-'+i,src).then(function(r){var d=document.createElement('div');d.className='mdv-mermaid';d.innerHTML=r.svg;host.replaceWith(d);return {d:d,src:src,i:i};}).catch(function(){return null;}));
        });
      });
      if(dark)Promise.all(jobs).then(function(done){
        window.mermaid.initialize({startOnLoad:false,theme:'default',securityLevel:'strict'});
        return done.filter(Boolean).reduce(function(p,j){
          return p.then(function(){return window.mermaid.render('mdv-mmdp-'+j.i,j.src).then(function(r){var d=document.createElement('div');d.className='mdv-mermaid mdv-mermaid-print';d.innerHTML=r.svg;j.d.classList.add('mdv-mermaid-screen');j.d.after(d);}).catch(function(){});});
        },Promise.resolve());
      }).catch(function(){});
    });
  });
}
if(mode==='rendered'&&root.dataset.math==='1'){
  load('/__mdv/vendor/katex.min.js',function(err){
    safe(function(){
      if(err||!window.katex||typeof window.katex.render!=='function')return;
      var l=document.createElement('link');l.rel='stylesheet';l.href='/__mdv/vendor/katex.min.css';document.head.appendChild(l);
      document.querySelectorAll('[data-math-style]').forEach(function(el){
        safe(function(){window.katex.render(el.textContent,el,{displayMode:el.dataset.mathStyle==='display',throwOnError:false});});
      });
    });
  });
}
})();
