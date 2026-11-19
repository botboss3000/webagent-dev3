'use strict';
import { createSubpageLinks } from '../subpage-list/subpage-list.js';

/** The pin control for one page: pressed keeps it in the tab strip. */
function starButton(page, favorites, onToggle) {
  const star = document.createElement('button');
  star.type = 'button';
  star.className = 'agent-config-topic-icon agent-config-star';
  star.setAttribute('aria-label', `Star ${page.title}`);
  const selected = favorites.includes(page.id);
  star.dataset.favoritePage = page.id;
  star.setAttribute('aria-pressed', String(selected));
  star.addEventListener('click', () => onToggle(page.id));
  star.innerHTML = '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m12 3 2.78 5.63L21 9.54l-4.5 4.39 1.06 6.2L12 17.2l-5.56 2.93 1.06-6.2L3 9.54l6.22-.91L12 3z"/></svg>';
  star.querySelector('svg').setAttribute('fill', selected ? 'currentColor' : 'none');
  return star;
}

/** A Config tab: each page is a link row that opens it, with a star that pins it to the tabs.
 *  Used by the Agents and App tabs; labels name the directory, its group and its list. */
export function renderPageDirectory(host, pages, onNavigate, favorites = [], onToggle = () => {},
  {label = 'Page directory', group: groupLabel = 'Core configuration', list = 'Configuration pages'} = {}) {
  const landing = document.createElement('div');
  landing.className = 'agent-config-landing';
  landing.setAttribute('aria-label', label);
  const group = document.createElement('section');
  group.className = 'agent-config-settings-group';
  const heading = document.createElement('div');
  heading.className = 'agent-config-settings-group-label';
  heading.textContent = groupLabel;
  group.append(heading);
  createSubpageLinks(group, {
    label: list,
    items: pages.map(page => ({
      id: page.id,
      title: page.title,
      hint: page.description || '',
      icon: page.icon,
      trailing: starButton(page, favorites, onToggle),
      onSelect: onNavigate,
    })),
  });
  landing.append(group);
  host.replaceChildren(landing);
}
