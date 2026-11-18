'use strict';
import { createSettingsMenu } from '../../../../shared/settings-register/settings-register.js';

/** The pin control for one page: pressed keeps it in the agent card tabs. */
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

/** The Config tab: each agent page is a register row that opens that page. */
export function renderAgentDirectory(host, pages, onNavigate, favorites = [], onToggle = () => {}) {
  const landing = document.createElement('div');
  landing.className = 'agent-config-landing';
  landing.setAttribute('aria-label', 'Agent page directory');
  const group = document.createElement('section');
  group.className = 'agent-config-settings-group';
  const label = document.createElement('div');
  label.className = 'agent-config-settings-group-label';
  label.textContent = 'Core configuration';
  group.append(label);
  createSettingsMenu(group, {
    label: 'Agent configuration pages',
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
