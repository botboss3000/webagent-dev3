'use strict';
import { renderPageDirectory } from '../../../../shared/page-directory/page-directory.js';

/** The Agents Config tab: the shared page directory with the agent labels. */
export function renderAgentDirectory(host, pages, onNavigate, favorites = [], onToggle = () => {}) {
  renderPageDirectory(host, pages, onNavigate, favorites, onToggle,
    {label: 'Agent page directory', list: 'Agent configuration pages'});
}
