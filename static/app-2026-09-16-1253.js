/* 
   NB: API_BASE and PHOTO_BASE are virtual path names handled by the web server
   and translated into the real paths to the application server and the photos 
   folder tree respectively. If these names clash with any existing real folder 
   names on the website then change them here AND in the web server configuration file.
*/
const API_BASE = '/api';
const PHOTO_BASE = '/photoalbum'; 

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
    document.getElementById('share-folder').classList.remove('hidden');
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
        card.className = 'card card-folder';
        const folderPathPrefix = folder.path ? encodePath(folder.path) + '/' : '';
        const thumbSrc = folder.cover ? `${PHOTO_BASE}/${folderPathPrefix}${folder.cover}` : '';
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
        const thumbSrc = `${PHOTO_BASE}/${encodePath(currentPath)}/${photo.thumb}`;
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
    const src = `${PHOTO_BASE}/${encodePath(currentPath)}/${encodeURIComponent(photo.name)}`;
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
        img.decoding = 'async';
        content.appendChild(img);
    }
    preloadAdjacentImages();
}

function preloadAdjacentImages() {
    if (!currentAlbum || currentAlbum.photos.length === 0) return;

    const indices = [];
    if (currentViewerIndex > 0) indices.push(currentViewerIndex - 1);
    if (currentViewerIndex < currentAlbum.photos.length - 1) indices.push(currentViewerIndex + 1);

    for (const idx of indices) {
        const photo = currentAlbum.photos[idx];
        if (photo.type === 'video') continue; // skip video preloading
        const src = `${PHOTO_BASE}/${encodePath(currentPath)}/${encodeURIComponent(photo.name)}`;

        // Use a hidden img element to force download + decode in the background
        let preloader = document.getElementById(`preload-${idx}`);
        if (!preloader) {
            preloader = document.createElement('img');
            preloader.id = `preload-${idx}`;
            preloader.style.cssText = 'position:absolute;width:1px;height:1px;opacity:0;pointer-events:none;';
            preloader.decoding = 'async';
            document.body.appendChild(preloader);
        }
        preloader.src = src;
    }

    // Clean up preloaders that are no longer adjacent
    const adjacentIds = new Set(indices.map(i => `preload-${i}`));
    document.querySelectorAll('img[id^="preload-"]').forEach(el => {
        if (!adjacentIds.has(el.id)) {
            el.remove();
        }
    });
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
    const src = `${PHOTO_BASE}/${encodePath(currentPath)}/${encodeURIComponent(photo.name)}`;
    const a = document.createElement('a');
    a.href = src;
    a.download = photo.name;
    a.click();
}

// ---------------------------------------------------------------------------
// Sharing
//
// Two things can be shared:
//   - a folder  -> the page URL, which the SPA reopens via its #path= hash
//   - a photo   -> the direct media URL under PHOTO_BASE
// Both are produced from the same sheet: copy to clipboard, hand off to the
// OS share sheet, or open a platform's share/intent endpoint.
// ---------------------------------------------------------------------------

// `color` is the brand dot shown beside the label. `imageOnly` targets are
// hidden when there is no image to attach (i.e. when sharing a folder).
const SHARE_PLATFORMS = [
    { id: 'email',     label: 'Email',     color: '#6b7280' },
    { id: 'whatsapp',  label: 'WhatsApp',  color: '#25d366' },
    { id: 'facebook',  label: 'Facebook',  color: '#1877f2' },
    { id: 'x',         label: 'X',         color: '#111111' },
    { id: 'telegram',  label: 'Telegram',  color: '#26a5e4' },
    { id: 'pinterest', label: 'Pinterest', color: '#e60023', imageOnly: true },
];

let shareTarget = null;

// The site name shown in share text, taken from the visible header heading so
// it matches what the visitor sees (falls back to the document title).
function siteName() {
    const h1 = document.querySelector('header h1');
    return (h1 && h1.textContent.trim()) || document.title;
}

function shareText(label) {
    const site = siteName();
    return label && label !== site ? `${label} \u2014 ${site}` : site;
}

// Page URL for a folder. The empty (root) path yields a bare page URL rather
// than a dangling "#path=".
function folderPageUrl(path) {
    const base = `${window.location.origin}${window.location.pathname}`;
    return path ? `${base}#path=${encodeURIComponent(path)}` : base;
}

function photoMediaUrl(photo) {
    return `${window.location.origin}${PHOTO_BASE}/${encodePath(currentPath)}/${encodeURIComponent(photo.name)}`;
}

function platformShareUrl(id, target) {
    const url = encodeURIComponent(target.url);
    const text = encodeURIComponent(target.text);
    const both = encodeURIComponent(`${target.text} ${target.url}`);
    switch (id) {
        case 'email':
            return `mailto:?subject=${text}&body=${encodeURIComponent(`${target.text}\n\n${target.url}`)}`;
        case 'whatsapp':
            return `https://wa.me/?text=${both}`;
        case 'facebook':
            return `https://www.facebook.com/sharer/sharer.php?u=${url}`;
        case 'x':
            return `https://x.com/intent/post?url=${url}&text=${text}`;
        case 'telegram':
            return `https://t.me/share/url?url=${url}&text=${text}`;
        case 'pinterest':
            return `https://www.pinterest.com/pin/create/button/?url=${url}&media=${encodeURIComponent(target.imageUrl || target.url)}&description=${text}`;
        default:
            return target.url;
    }
}

function openShareSheet(target) {
    shareTarget = target;
    document.getElementById('share-url').value = target.url;
    document.getElementById('share-subtitle').textContent = target.label || '';

    const container = document.getElementById('share-targets');
    container.innerHTML = '';

    // Native OS share sheet first, when the browser offers one (mostly mobile).
    if (navigator.share) {
        const native = document.createElement('button');
        native.type = 'button';
        native.className = 'share-target';
        native.innerHTML = '<span class="share-dot" style="background:var(--accent)"></span>Share\u2026';
        native.addEventListener('click', () => {
            // Fires the user's OS sheet; failures (e.g. user cancelled) are not
            // worth surfacing, since the sheet is dismissed either way.
            navigator.share({ title: siteName(), text: target.text, url: target.url }).catch(() => {});
            closeShareSheet();
        });
        container.appendChild(native);
    }

    for (const platform of SHARE_PLATFORMS) {
        if (platform.imageOnly && !target.imageUrl) continue;
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'share-target';
        btn.innerHTML = `<span class="share-dot" style="background:${platform.color}"></span>${escapeHtml(platform.label)}`;
        btn.addEventListener('click', () => {
            const shareUrl = platformShareUrl(platform.id, target);
            // mailto must not go through window.open, or some browsers leave an
            // empty tab behind.
            if (platform.id === 'email') {
                window.location.href = shareUrl;
            } else {
                window.open(shareUrl, '_blank', 'noopener,noreferrer');
            }
            closeShareSheet();
        });
        container.appendChild(btn);
    }

    document.getElementById('share-sheet').classList.remove('hidden');
}

function closeShareSheet() {
    document.getElementById('share-sheet').classList.add('hidden');
    shareTarget = null;
}

// Kept open on purpose so the URL stays visible for manual copying if the
// clipboard is unavailable (e.g. a non-HTTPS origin).
async function copyShareUrl() {
    if (!shareTarget) return;
    const url = shareTarget.url;
    try {
        await navigator.clipboard.writeText(url);
        showToast('Link copied');
    } catch (err) {
        const input = document.getElementById('share-url');
        input.focus();
        input.select();
        let ok = false;
        try {
            ok = document.execCommand('copy');
        } catch (e) {
            ok = false;
        }
        showToast(ok ? 'Link copied' : 'Copy failed \u2014 select the link and copy manually');
    }
}

function shareFolder() {
    if (!currentAlbum) return;
    const crumbs = currentAlbum.breadcrumbs || [];
    const label = currentPath && crumbs.length ? crumbs[crumbs.length - 1].name : '';
    openShareSheet({
        url: folderPageUrl(currentPath),
        text: shareText(label),
        label: label || siteName(),
    });
}

function sharePhoto() {
    const photo = currentAlbum.photos[currentViewerIndex];
    const url = photoMediaUrl(photo);
    openShareSheet({
        url,
        imageUrl: url,
        text: shareText(photo.name),
        label: photo.name,
    });
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
    // While the share sheet is open it owns the keyboard: Escape closes it, and
    // viewer navigation must not fire underneath it.
    const sheet = document.getElementById('share-sheet');
    if (!sheet.classList.contains('hidden')) {
        if (e.key === 'Escape') closeShareSheet();
        return;
    }

    const viewer = document.getElementById('viewer');
    if (viewer.classList.contains('hidden')) return;
    if (e.key === 'ArrowLeft') viewerPrev();
    if (e.key === 'ArrowRight') viewerNext();
    if (e.key === 'Escape') closeViewer();
});

// Swipe navigation (mobile)
(function initSwipe() {
    const content = document.getElementById('viewer-content');
    let startX = 0;
    let startY = 0;
    const threshold = 50;

    content.addEventListener('touchstart', e => {
        startX = e.changedTouches[0].screenX;
        startY = e.changedTouches[0].screenY;
    }, { passive: true });

    content.addEventListener('touchend', e => {
        const endX = e.changedTouches[0].screenX;
        const endY = e.changedTouches[0].screenY;
        const dx = endX - startX;
        const dy = endY - startY;

        // Only act on clear horizontal swipes
        if (Math.abs(dx) > Math.abs(dy) && Math.abs(dx) > threshold) {
            if (dx < 0) {
                viewerNext();   // swipe left → next
            } else {
                viewerPrev();   // swipe right → previous
            }
        }
    }, { passive: true });
})();

// Event bindings
document.getElementById('viewer-close').addEventListener('click', closeViewer);
document.getElementById('viewer-up').addEventListener('click', () => {
    hideViewer();
});
document.getElementById('viewer-prev').addEventListener('click', viewerPrev);
document.getElementById('viewer-next').addEventListener('click', viewerNext);
document.getElementById('viewer-download').addEventListener('click', viewerDownload);
document.getElementById('viewer-share').addEventListener('click', sharePhoto);
document.getElementById('share-folder').addEventListener('click', shareFolder);
document.getElementById('share-copy').addEventListener('click', copyShareUrl);
document.getElementById('share-close').addEventListener('click', closeShareSheet);
document.getElementById('share-sheet').addEventListener('click', e => {
    // Backdrop click dismisses; clicks inside the panel do not.
    if (e.target === e.currentTarget) closeShareSheet();
});
document.getElementById('cover-cancel').addEventListener('click', closeCoverModal);
document.getElementById('cover-confirm').addEventListener('click', confirmCover);

// Init — read path from URL hash BEFORE admin init clears it
const initialPath = getPathFromHash();
initAdmin();
initTheme();
loadAlbum(initialPath);
