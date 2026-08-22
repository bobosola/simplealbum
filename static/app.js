const API_BASE = '/api';

let currentPath = '';
let currentAlbum = null;
let currentViewerIndex = -1;
let adminKey = localStorage.getItem('album_admin_key') || '';

// Admin mode from fragment
function initAdmin() {
    const hash = window.location.hash;
    if (hash.startsWith('#admin=')) {
        adminKey = hash.slice(7);
        localStorage.setItem('album_admin_key', adminKey);
        history.replaceState(null, '', window.location.pathname + window.location.search);
    }
    const badge = document.getElementById('admin-badge');
    if (adminKey) {
        badge.classList.remove('hidden');
        badge.style.cursor = 'pointer';
        badge.addEventListener('click', () => {
            localStorage.removeItem('album_admin_key');
            adminKey = '';
            showToast('Admin mode exited');
            setTimeout(() => location.reload(), 500);
        });
    }
}

// Theme
function initTheme() {
    const saved = localStorage.getItem('album_theme');
    const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
    const isDark = saved === 'dark' || (!saved && prefersDark);
    if (isDark) document.documentElement.setAttribute('data-theme', 'dark');
    document.getElementById('theme-toggle').addEventListener('click', () => {
        const isDark = document.documentElement.getAttribute('data-theme') === 'dark';
        if (isDark) {
            document.documentElement.removeAttribute('data-theme');
            localStorage.setItem('album_theme', 'light');
        } else {
            document.documentElement.setAttribute('data-theme', 'dark');
            localStorage.setItem('album_theme', 'dark');
        }
    });
}

// Toast
function showToast(msg) {
    const toast = document.getElementById('toast');
    toast.textContent = msg;
    toast.classList.remove('hidden');
    setTimeout(() => toast.classList.add('hidden'), 2500);
}

// History management
function getPathFromHash() {
    const hash = window.location.hash;
    if (hash.startsWith('#path=')) {
        return decodeURIComponent(hash.slice(6));
    }
    return '';
}

function navigateTo(path) {
    hideViewer();
    history.pushState({path}, '', '#path=' + encodeURIComponent(path));
    loadAlbum(path);
}

// Load album (no history touch — history is managed by navigateTo/popstate)
async function loadAlbum(path) {
    currentPath = path;
    const res = await fetch(`${API_BASE}/album?path=${encodeURIComponent(path)}`);
    if (!res.ok) {
        showToast('Failed to load album');
        return;
    }
    currentAlbum = await res.json();
    renderBreadcrumbs();
    renderGrid();
}

// Breadcrumbs
function renderBreadcrumbs() {
    const nav = document.getElementById('breadcrumbs');
    if (!currentAlbum || !currentAlbum.breadcrumbs) {
        nav.innerHTML = '';
        return;
    }
    const parts = currentAlbum.breadcrumbs.map((crumb, i) => {
        if (i === currentAlbum.breadcrumbs.length - 1) {
            return `<span>${escapeHtml(crumb.name)}</span>`;
        }
        return `<a data-path="${escapeHtml(crumb.path)}">${escapeHtml(crumb.name)}</a>`;
    });
    nav.innerHTML = parts.join('<span class="separator">/</span>');
    nav.querySelectorAll('a').forEach(a => {
        a.addEventListener('click', e => {
            e.preventDefault();
            navigateTo(a.dataset.path);
        });
    });
}

// Grid
function renderGrid() {
    const grid = document.getElementById('grid');
    grid.innerHTML = '';
    if (!currentAlbum) return;

    // Folders
    for (const folder of currentAlbum.folders) {
        const card = document.createElement('div');
        card.className = 'card';
        const folderPathPrefix = folder.path ? encodePath(folder.path) + '/' : '';
        const thumbSrc = folder.cover ? `/photoalbum/${folderPathPrefix}${folder.cover}` : '';
        card.innerHTML = `
            <div class="thumb-wrap">
                ${thumbSrc ? `<img src="${thumbSrc}" loading="lazy" alt="">` : '<div class="placeholder"></div>'}
            </div>
            <div class="info">
                <div class="name">${escapeHtml(folder.name)}</div>
                <div class="counts">${folder.count_photos} photos${folder.count_albums ? ', ' + folder.count_albums + ' albums' : ''}</div>
            </div>
        `;
        card.addEventListener('click', () => navigateTo(folder.path));
        grid.appendChild(card);
    }

    // Photos / Videos
    for (let i = 0; i < currentAlbum.photos.length; i++) {
        const photo = currentAlbum.photos[i];
        const card = document.createElement('div');
        card.className = 'card';
        const thumbSrc = `/photoalbum/${encodePath(currentPath)}/${photo.thumb}`;
        const isVideo = photo.type === 'video';
        card.innerHTML = `
            <div class="thumb-wrap">
                <img src="${thumbSrc}" loading="lazy" alt="${escapeHtml(photo.name)}">
                ${isVideo ? '<div class="play-icon"></div>' : ''}
                ${adminKey ? `<button class="set-cover-btn" data-index="${i}" title="Set as cover">&#9733;</button>` : ''}
            </div>
            <div class="info">
                <div class="name">${escapeHtml(photo.name)}</div>
                <div class="counts">${photo.width || 0}x${photo.height || 0}${isVideo && photo.duration ? ' &middot; ' + formatDuration(photo.duration) : ''}</div>
            </div>
        `;
        card.addEventListener('click', (e) => {
            if (e.target.closest('.set-cover-btn')) return;
            openViewer(i);
        });
        const btn = card.querySelector('.set-cover-btn');
        if (btn) {
            btn.addEventListener('click', (e) => {
                e.stopPropagation();
                openCoverModal(photo);
            });
        }
        grid.appendChild(card);
    }
}

function formatDuration(sec) {
    const m = Math.floor(sec / 60);
    const s = sec % 60;
    return `${m}:${s.toString().padStart(2, '0')}`;
}

// Viewer
function openViewer(index) {
    currentViewerIndex = index;
    const viewer = document.getElementById('viewer');
    viewer.classList.remove('hidden');
    document.body.classList.add('viewer-open');
    renderViewerItem();
    // Push history state so browser back button closes the viewer
    history.pushState({path: currentPath, view: index}, '');
}

function closeViewer() {
    history.back();
}

function stopViewerVideo() {
    const content = document.getElementById('viewer-content');
    const video = content.querySelector('video');
    if (video) {
        video.pause();
        video.removeAttribute('src');
        video.load(); // force decoder release
    }
}

function renderViewerItem() {
    stopViewerVideo();
    const photo = currentAlbum.photos[currentViewerIndex];
    const content = document.getElementById('viewer-content');
    const src = `/photoalbum/${encodePath(currentPath)}/${encodeURIComponent(photo.name)}`;
    content.innerHTML = '';
    if (photo.type === 'video') {
        const video = document.createElement('video');
        video.src = src;
        video.controls = true;
        video.autoplay = true;
        content.appendChild(video);
    } else {
        const img = document.createElement('img');
        img.src = src;
        img.alt = photo.name;
        content.appendChild(img);
    }
}

function viewerPrev() {
    if (currentViewerIndex > 0) {
        currentViewerIndex--;
        renderViewerItem();
    }
}

function viewerNext() {
    if (currentViewerIndex < currentAlbum.photos.length - 1) {
        currentViewerIndex++;
        renderViewerItem();
    }
}

function viewerUp() {
    document.getElementById('viewer').classList.add('hidden');
}

function hideViewer() {
    stopViewerVideo();
    document.getElementById('viewer').classList.add('hidden');
    document.body.classList.remove('viewer-open');
}

function viewerDownload() {
    const photo = currentAlbum.photos[currentViewerIndex];
    const src = `/photoalbum/${encodePath(currentPath)}/${encodeURIComponent(photo.name)}`;
    const a = document.createElement('a');
    a.href = src;
    a.download = photo.name;
    a.click();
}

function viewerCopy() {
    const photo = currentAlbum.photos[currentViewerIndex];
    const url = `${window.location.origin}/photoalbum/${encodePath(currentPath)}/${encodeURIComponent(photo.name)}`;
    navigator.clipboard.writeText(url).then(() => showToast('Link copied'));
}

// Cover modal
let coverModalPhoto = null;

function openCoverModal(photo) {
    coverModalPhoto = photo;
    const modal = document.getElementById('cover-modal');
    const container = document.getElementById('cover-checkboxes');
    container.innerHTML = '';

    const folders = [];
    let accum = '';
    for (const part of currentPath.split('/').filter(p => p)) {
        accum = accum ? `${accum}/${part}` : part;
        folders.push({ name: part, path: accum });
    }

    for (const folder of folders) {
        const label = document.createElement('label');
        const checkbox = document.createElement('input');
        checkbox.type = 'checkbox';
        checkbox.value = folder.path;
        checkbox.checked = folder.path === currentPath;
        label.appendChild(checkbox);
        label.appendChild(document.createTextNode(escapeHtml(folder.name)));
        container.appendChild(label);
    }

    modal.classList.remove('hidden');
}

async function confirmCover() {
    if (!coverModalPhoto || !adminKey) return;
    const checkboxes = document.querySelectorAll('#cover-checkboxes input[type="checkbox"]:checked');
    const targets = Array.from(checkboxes).map(cb => cb.value);
    if (targets.length === 0) {
        closeCoverModal();
        return;
    }
    const imagePath = currentPath ? `${currentPath}/${coverModalPhoto.name}` : coverModalPhoto.name;
    console.log('Setting cover:', { imagePath, targets });
    const res = await fetch(`${API_BASE}/cover`, {
        method: 'POST',
        headers: {
            'Content-Type': 'application/json',
            'X-Admin-Key': adminKey,
        },
        body: JSON.stringify({ image_path: imagePath, targets }),
    });
    console.log('Cover response:', res.status);
    if (res.ok) {
        showToast('Cover set');
        await loadAlbum(currentPath);
    } else if (res.status === 403) {
        showToast('Admin key invalid');
    } else {
        const text = await res.text();
        console.error('Cover error:', res.status, text);
        showToast('Failed to set cover');
    }
    closeCoverModal();
}

function closeCoverModal() {
    document.getElementById('cover-modal').classList.add('hidden');
    coverModalPhoto = null;
}

// Helpers
function escapeHtml(text) {
    const div = document.createElement('div');
    div.textContent = text;
    return div.innerHTML;
}

function encodePath(path) {
    return path.split('/').map(encodeURIComponent).join('/');
}

// History: handle browser back/forward buttons
window.addEventListener('popstate', e => {
    const state = e.state;
    const viewer = document.getElementById('viewer');

    // Handle viewer open/close transitions
    if (state && state.view !== undefined) {
        // State has a view index — open or update the viewer
        if (viewer.classList.contains('hidden')) {
            currentViewerIndex = state.view;
            viewer.classList.remove('hidden');
            document.body.classList.add('viewer-open');
            renderViewerItem();
        } else if (state.view !== currentViewerIndex) {
            currentViewerIndex = state.view;
            renderViewerItem();
        }
        return;
    }

    // State has no view — close viewer if open, then load album
    if (!viewer.classList.contains('hidden')) {
        hideViewer();
    }

    const path = state?.path ?? getPathFromHash();
    if (path !== currentPath) {
        loadAlbum(path);
    }
});

// Keyboard shortcuts
document.addEventListener('keydown', e => {
    const viewer = document.getElementById('viewer');
    if (viewer.classList.contains('hidden')) return;
    if (e.key === 'ArrowLeft') viewerPrev();
    if (e.key === 'ArrowRight') viewerNext();
    if (e.key === 'Escape') closeViewer();
});

// Event bindings
document.getElementById('viewer-close').addEventListener('click', closeViewer);
document.getElementById('viewer-up').addEventListener('click', () => {
    hideViewer();
});
document.getElementById('viewer-prev').addEventListener('click', viewerPrev);
document.getElementById('viewer-next').addEventListener('click', viewerNext);
document.getElementById('viewer-download').addEventListener('click', viewerDownload);
document.getElementById('viewer-copy').addEventListener('click', viewerCopy);
document.getElementById('cover-cancel').addEventListener('click', closeCoverModal);
document.getElementById('cover-confirm').addEventListener('click', confirmCover);

// Init — read path from URL hash BEFORE admin init clears it
const initialPath = getPathFromHash();
initAdmin();
initTheme();
loadAlbum(initialPath);
