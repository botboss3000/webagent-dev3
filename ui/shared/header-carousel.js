"use strict";
import { applyRubberBand } from './rubber-band.js';
export function initHeaderCarousel() {
const tabBar=document.getElementById("main-tabs"), tabWrap=document.getElementById("main-tabs-wrap"), chevLeft=document.getElementById("main-tabs-chev-left"), chevRight=document.getElementById("main-tabs-chev-right");
  function updateChevrons() {
    if (!chevLeft || !chevRight) return;
    var overflow = tabBar.scrollWidth - tabBar.clientWidth > 1;
    tabWrap.classList.toggle('has-overflow', overflow);
    chevLeft.classList.toggle('visible', overflow && tabBar.scrollLeft > 1);
    chevRight.classList.toggle('visible', overflow && tabBar.scrollLeft < tabBar.scrollWidth - tabBar.clientWidth - 1);
  }

  function syncActive(tabId) {
    tabBar.querySelectorAll('.main-tab').forEach(function (b) {
      b.classList.toggle('active', b.dataset.value === tabId);
    });
    var activeBtn = tabBar.querySelector('.main-tab.active');
    if (activeBtn) activeBtn.scrollIntoView({ behavior: 'smooth', inline: 'center', block: 'nearest' });
    requestAnimationFrame(updateChevrons);
    setTimeout(updateChevrons, 350); // catch end of smooth scroll
  }

  // Chevron carousel (matches app-config tabs pattern)
  if (chevLeft && chevRight) {
    var scrollStep = function () { return Math.max(80, Math.floor(tabBar.clientWidth * 0.6)); };
    chevLeft.addEventListener('click', function () { tabBar.scrollBy({ left: -scrollStep(), behavior: 'smooth' }); });
    chevRight.addEventListener('click', function () { tabBar.scrollBy({ left: scrollStep(), behavior: 'smooth' }); });
    tabBar.addEventListener('scroll', updateChevrons, { passive: true });

    requestAnimationFrame(function () {
      updateChevrons();
      var active = tabBar.querySelector('.main-tab.active');
      if (active) active.scrollIntoView({ inline: 'center', block: 'nearest' });
    });
    if (typeof ResizeObserver !== 'undefined') {
      var roPending = false;
      var ro = new ResizeObserver(function () {
        if (roPending) return;
        roPending = true;
        requestAnimationFrame(function () { roPending = false; updateChevrons(); });
      });
      ro.observe(tabBar);
      ro.observe(tabWrap);
    }
    window.addEventListener('resize', updateChevrons);
  }

  applyRubberBand(tabBar);


let last="";
function refresh(){const active=tabBar.querySelector(".main-tab.active");const id=active?.dataset.value;if(id && id!==last){last=id;syncActive(id);}else updateChevrons();}
new MutationObserver(refresh).observe(tabBar,{subtree:true,childList:true,attributes:true,attributeFilter:["class"]});
window.__refreshTabStrip=refresh; refresh();
}
