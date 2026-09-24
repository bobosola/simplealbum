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

// Admin mode from fragment.
//
// `adoptAdminKeyFromHash` is separate from `initAdmin` because it has to be
// callable more than once: entering a URL that differs only in its *fragment* —
// which is exactly what pasting an admin link into a tab that already has the
// album open does — is a same-document navigation. The browser swaps the
// fragment and fires `hashchange` **without reloading the page**, so anything
// that only runs at load time never runs, and admin mode silently never turns
// on. (Chrome behaves this way; the Safari attempt happened to be a real page
// load, which is why it worked.) Returns true when a fragment was consumed.
function adoptAdminKeyFromHash() {
    const hash = window.location.hash;
    if (!hash.startsWith('#admin=')) return false;
    adminKey = hash.slice(7);
    localStorage.setItem('album_admin_key', adminKey);
    // Strip it immediately: it must not reach history, `Referer`, or a URL the
    // user later copies out of the address bar.
    history.replaceState(null, '', window.location.pathname + window.location.search);
    return true;
}

// The badge reflects whether a key is held, so it can appear mid-session.
function refreshAdminBadge() {
    document.getElementById('admin-badge').classList.toggle('hidden', !adminKey);
}

function initAdmin() {
    const badge = document.getElementById('admin-badge');
    badge.style.cursor = 'pointer';
    badge.addEventListener('click', () => {
        localStorage.removeItem('album_admin_key');
        adminKey = '';
        showToast('Admin mode exited');
        setTimeout(() => location.reload(), 500);
    });
    adoptAdminKeyFromHash();
    refreshAdminBadge();
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
// The path from a `#path=` fragment, decoded. Returns `null` when the fragment
// is something else (`#admin=…`, or no fragment at all) so callers can tell
// "the root folder was requested" apart from "no folder was requested".
function getPathFromHash() {
    const hash = window.location.hash;
    if (hash.startsWith('#path=')) {
        return decodeURIComponent(hash.slice(6));
    }
    return null;
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
        const thumbSrc = folder.cover ? photoUrl(folder.cover, folder.path) : '';
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
        attachThumbFallback(card);
        grid.appendChild(card);
    }

    // Photos / Videos
    for (let i = 0; i < currentAlbum.photos.length; i++) {
        const photo = currentAlbum.photos[i];
        const card = document.createElement('div');
        card.className = 'card';
        const thumbSrc = photoUrl(photo.thumb);
        const isVideo = photo.type === 'video';
        card.innerHTML = `
            <div class="thumb-wrap">
                <img src="${thumbSrc}" loading="lazy" alt="${escapeHtml(photo.name)}">
                ${isVideo ? '<div class="play-icon"></div>' : ''}
                ${adminKey ? `<button class="set-cover-btn" data-index="${i}" title="Set as cover">&#9733;</button>` : ''}
            </div>
            <div class="info">
                <div class="name">${escapeHtml(photo.name)}</div>
                <div class="counts">${photo.width || 0}x${photo.height || 0}${isVideo && photo.duration != null ? ' &middot; ' + formatDuration(photo.duration) : ''}</div>
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
        attachThumbFallback(card);
        grid.appendChild(card);
    }
}

// Swap a thumbnail that fails to load for the same muted placeholder used when
// no cover is known. A thumbnail can legitimately be missing (the background
// worker has not reached the file yet), and the API does not report whether one
// exists, so the failure is detected in the browser instead. Without this the
// card shows a broken-image glyph. `error` listeners are used rather than an
// inline `onerror` attribute because the documented Content-Security-Policy
// forbids inline script.
function attachThumbFallback(card) {
    const img = card.querySelector('.thumb-wrap img');
    if (!img) return;
    const fallback = () => {
        if (!img.parentNode) return;
        const placeholder = document.createElement('div');
        placeholder.className = 'placeholder';
        img.replaceWith(placeholder);
    };
    img.addEventListener('error', fallback);
    // A cached failure can fire before the listener is attached.
    if (img.complete && img.naturalWidth === 0) fallback();
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
    // Landscape phone: spend the tap's activation on full screen, since that is
    // the orientation with the least room to spare. Anything else opens as-is.
    if (landscapePhone.matches && !fullscreenElement()) {
        requestFullscreen(fullscreenTarget);
    }
    // Push history state so browser back button closes the viewer
    history.pushState({path: currentPath, view: index}, '');
}

// Leaves the viewer by undoing the history entry `openViewer` pushed, so that
// browsing history stays consistent: every exit path (close button, Escape, the
// up arrow) goes through here. The earlier up-arrow handler hid the viewer
// without touching history, which left a stale entry behind and made the next
// browser Back press do nothing.
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
    // A stale index must not throw. `popstate` can hand back a viewer entry
    // that outlives the photo it points at (deleted on another device, or a
    // folder with fewer photos reloaded), and the exception left the viewer
    // stuck open with its previous content.
    if (!photo) {
        hideViewer();
        return;
    }
    const content = document.getElementById('viewer-content');
    const src = photoUrl(photo.name);
    content.innerHTML = '';
    // The previous <img> is gone, so any transform recorded against it is stale.
    zoomState.img = null;
    zoomReset();
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
        zoomState.img = img;
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
        const src = photoUrl(photo.name);

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

function hideViewer() {
    stopViewerVideo();
    document.getElementById('viewer').classList.add('hidden');
    document.body.classList.remove('viewer-open');
    zoomState.img = null;
    zoomReset();
    // Leaving the viewer must not strand the user in full screen.
    exitFullscreen();
}

// ---------------------------------------------------------------------------
// Full screen
//
// A phone held in landscape has a viewport only ~360px tall, and the browser
// chrome (status bar + URL bar) takes roughly a third of it. No CSS can reclaim
// that — the Fullscreen API is the only thing that hides it, which is why
// landscape was the one orientation where the viewer looked cramped.
//
// `requestFullscreen` needs transient user activation, so it can only be entered
// from a tap or click: the toolbar button, or the tap that opened the viewer.
// A bare orientationchange is not a gesture and would be rejected, which is why
// there is no "enter full screen when the phone turns" listener here.
const viewerEl = document.getElementById('viewer');
const fullscreenBtn = document.getElementById('viewer-fullscreen');

// Full screen is requested on <html>, not on the viewer, because the Fullscreen
// API renders only the fullscreen element's subtree. The share sheet and the
// toast are siblings of the viewer, so fullscreening the viewer would have made
// Share open an invisible panel behind the black. <html> contains everything.
const fullscreenTarget = document.documentElement;

// iPhone Safari has no element full screen (only video), so offer the button
// only where it can do something.
if (!document.fullscreenEnabled && !document.webkitFullscreenEnabled) {
    fullscreenBtn.classList.add('hidden');
}

function fullscreenElement() {
    return document.fullscreenElement || document.webkitFullscreenElement || null;
}

function requestFullscreen(el) {
    const fn = el.requestFullscreen || el.webkitRequestFullscreen;
    if (!fn) return;
    try {
        const p = fn.call(el);
        // A refused request (permissions policy, or another full screen element)
        // must not surface as an unhandled rejection; the viewer still works.
        if (p && p.catch) p.catch(() => {});
    } catch (_) { /* nothing else to do */ }
}

function exitFullscreen() {
    if (!fullscreenElement()) return;
    const fn = document.exitFullscreen || document.webkitExitFullscreen;
    if (!fn) return;
    try {
        const p = fn.call(document);
        if (p && p.catch) p.catch(() => {});
    } catch (_) { /* nothing else to do */ }
}

// Keeps the button in step with the real state, including when full screen is
// left by the browser itself (Escape, or a swipe from the top edge).
function syncFullscreenButton() {
    const on = fullscreenElement() === fullscreenTarget;
    viewerEl.dataset.fullscreen = on ? '1' : '';
    fullscreenBtn.setAttribute('aria-pressed', on ? 'true' : 'false');
    const label = on ? 'Exit full screen' : 'Full screen';
    fullscreenBtn.title = label;
    fullscreenBtn.setAttribute('aria-label', label);
}

function toggleFullscreen() {
    if (fullscreenElement() === fullscreenTarget) exitFullscreen();
    else requestFullscreen(fullscreenTarget);
}

// A phone lying on its side is the case the viewer was built for here, and the
// tap that opened the photo is valid activation, so full screen can be entered
// from there. The query keeps desktops and tablets out of it: they are in
// landscape by default, so entering full screen on every open would be a
// hijack, not a courtesy. A short *and* coarse-pointer viewport means a phone.
const landscapePhone = window.matchMedia(
    '(orientation: landscape) and (pointer: coarse) and (max-height: 520px)'
);

document.addEventListener('fullscreenchange', syncFullscreenButton);
document.addEventListener('webkitfullscreenchange', syncFullscreenButton);

function viewerDownload() {
    const photo = currentAlbum.photos[currentViewerIndex];
    if (!photo) return;
    const src = photoUrl(photo.name);
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
// `color` is gone: the dot colour is presentational and lives in the
// stylesheet as `.share-dot--<id>`, so the CSP does not need
// `style-src 'unsafe-inline'` for it. `id` doubles as the class suffix.
const SHARE_PLATFORMS = [
    { id: 'email',     label: 'Email' },
    { id: 'whatsapp',  label: 'WhatsApp' },
    { id: 'facebook',  label: 'Facebook' },
    { id: 'x',         label: 'X' },
    { id: 'telegram',  label: 'Telegram' },
    { id: 'pinterest', label: 'Pinterest', imageOnly: true },
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

// Shareable URL for a folder or a single photo.
//
// These point at /api/share rather than at the SPA URL. A crawler builds a link
// preview from Open Graph tags in the HTML it fetches, and it never runs
// JavaScript -- so the SPA's own "#path=..." fragment, which is never sent to
// the server, gives every shared link the same generic preview. /api/share takes
// the same information as a query string, which does reach the server, so each
// folder and photo previews with its own title and thumbnail. It then forwards a
// human visitor straight on to the real destination.
function shareUrl(path, photoName) {
    const params = new URLSearchParams();
    if (path) params.set('path', path);
    if (photoName) params.set('photo', photoName);
    return `${window.location.origin}${API_BASE}/share?${params.toString()}`;
}

function photoMediaUrl(photo) {
    return `${window.location.origin}${photoUrl(photo.name)}`;
}

// Absolute URL of a still image representing `photo`, for share targets that
// only accept an image. For a video that is its generated poster frame — the
// video's own URL is not an image and Pinterest rejects it. Absolute, because
// Pinterest fetches it from its own servers.
function photoShareImageUrl(photo) {
    const rel = photo.type === 'video' ? photo.thumb : photo.name;
    return `${window.location.origin}${photoUrl(rel)}`;
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
        native.innerHTML = '<span class="share-dot share-dot--accent"></span>Share\u2026';
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
        btn.innerHTML = `<span class="share-dot share-dot--${platform.id}"></span>${escapeHtml(platform.label)}`;
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
        url: shareUrl(currentPath),
        text: shareText(label),
        label: label || siteName(),
    });
}

function sharePhoto() {
    const photo = currentAlbum.photos[currentViewerIndex];
    // Defensive: the button only exists inside the viewer, but a stale index
    // (the photo was deleted on another device) must not throw here.
    if (!photo) return;
    openShareSheet({
        url: shareUrl(currentPath, photo.name),
        // Pinterest wants a direct image file; the share page is HTML.
        imageUrl: photoShareImageUrl(photo),
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

    // The album root is the empty path and is offered alongside every ancestor
    // of the image, so any photo can also become the cover of the whole album.
    // It is not derivable from `currentPath` (which is empty when the root
    // itself is being browsed, leaving the list otherwise blank).
    const folders = [{ name: 'Home', path: '' }];
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
        // Plain text node: no HTML escaping here, or the entities would be
        // shown literally (`Fish &amp; Chips`).
        label.appendChild(document.createTextNode(folder.name));
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
    const res = await fetch(`${API_BASE}/cover`, {
        method: 'POST',
        headers: {
            'Content-Type': 'application/json',
            'X-Admin-Key': adminKey,
        },
        body: JSON.stringify({ image_path: imagePath, targets }),
    });
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
//
// Escapes for use in both text and quoted attribute values. The `div.innerHTML`
// trick escapes only `&`, `<` and `>`, which is enough for text nodes but not
// for the `alt="..."` and `data-path="..."` attributes this file also builds:
// an unescaped double quote in a filename would close the attribute and let the
// rest of the name inject markup. Quotes are therefore escaped explicitly.
function escapeHtml(text) {
    return String(text)
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;')
        .replace(/'/g, '&#39;');
}

function encodePath(path) {
    return path.split('/').map(encodeURIComponent).join('/');
}

// URL under PHOTO_BASE for a path relative to `base` (the folder the item lives
// under, defaulting to the folder being viewed).
//
// Segments are joined explicitly because the obvious template
// `${PHOTO_BASE}/${base}/${rel}` produces a doubled slash at the album root, and
// every segment is percent-encoded so that a filename containing '#', '?' or a
// space cannot truncate the URL or break the request.
function photoUrl(rel, base = currentPath) {
    const segments = [base, rel].filter(p => p).map(encodePath).join('/');
    return `${PHOTO_BASE}/${segments}`;
}

// History: handle browser back/forward buttons
window.addEventListener('popstate', e => {
    const state = e.state;
    const viewer = document.getElementById('viewer');

    // Handle viewer open/close transitions
    if (state && state.view !== undefined) {
        const index = state.view;
        // Only restore a viewer entry whose photo still exists in the loaded
        // album. Otherwise (the photo was deleted, or a different folder is
        // loaded) there is nothing to show, so leave the viewer closed instead
        // of rendering an undefined entry.
        if (
            !currentAlbum ||
            !Number.isInteger(index) ||
            index < 0 ||
            index >= currentAlbum.photos.length
        ) {
            hideViewer();
            return;
        }
        // State has a view index — open or update the viewer
        if (viewer.classList.contains('hidden')) {
            currentViewerIndex = index;
            viewer.classList.remove('hidden');
            document.body.classList.add('viewer-open');
            renderViewerItem();
        } else if (index !== currentViewerIndex) {
            currentViewerIndex = index;
            renderViewerItem();
        }
        return;
    }

    // State has no view — close viewer if open, then load album
    if (!viewer.classList.contains('hidden')) {
        hideViewer();
    }

    // A state-less entry with no `#path=` fragment (a bare album URL, or the
    // history entry the admin key was stripped from) carries no destination, so
    // leave the album where it is rather than bouncing to the root.
    const path = state?.path ?? getPathFromHash() ?? currentPath;
    if (path !== currentPath) {
        loadAlbum(path);
    }
});

// A fragment-only navigation does not reload the page, so `hashchange` is the
// only hook that runs when an admin link (or a deep link) is pasted into a tab
// that already has the album open. Handle both kinds of fragment here; the SPA's
// own navigation uses `pushState`, which never fires this.
window.addEventListener('hashchange', () => {
    if (adoptAdminKeyFromHash()) {
        refreshAdminBadge();
        // The "Set as cover" stars are rendered from `adminKey`, so a grid that
        // was drawn before the key arrived has to be redrawn for them to show.
        renderGrid();
        showToast('Admin mode enabled');
        return;
    }
    const path = getPathFromHash();
    if (path !== null && path !== currentPath) {
        hideViewer();
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
    if (e.key === 'Escape') {
        // While the viewer holds full screen the browser uses Escape to leave
        // it; closing the photo as well would take two things away at once.
        if (fullscreenElement() === fullscreenTarget) return;
        closeViewer();
    }
});

// ---------------------------------------------------------------------------
// Viewer zoom
//
// A page in full screen cannot be pinch-zoomed: Chrome on Android resets the
// viewport scale on entry and ignores the meta viewport's scale settings, and a
// page zoom would scale the toolbar along with the photo anyway. So the viewer
// scales the photo itself — pinch to zoom about the fingers, drag to pan,
// double-tap to toggle. Doing the gesture handling here is also what stops a
// pinch from being read as a swipe to the next photo.
const ZOOM_MAX = 5;
const ZOOM_DOUBLE_TAP = 2.5;
const zoomState = { scale: 1, tx: 0, ty: 0, img: null };

function zoomFrame() {
    return document.getElementById('viewer-content');
}

function zoomReset() {
    zoomState.scale = 1;
    zoomState.tx = 0;
    zoomState.ty = 0;
    if (zoomState.img) {
        zoomState.img.style.transition = '';
        zoomState.img.style.transform = '';
    }
}

// Bounds the pan by the displayed (object-fit: contain) size of the photo,
// scaled up, rather than by the element box: otherwise a letterboxed photo
// could be dragged off into the black beside it.
function zoomClamp() {
    const img = zoomState.img;
    const frame = zoomFrame();
    if (!img || !frame) return;
    if (zoomState.scale <= 1) {
        zoomState.scale = 1;
        zoomState.tx = 0;
        zoomState.ty = 0;
        return;
    }
    const r = frame.getBoundingClientRect();
    let drawW = r.width;
    let drawH = r.height;
    if (img.naturalWidth && img.naturalHeight) {
        const ar = img.naturalWidth / img.naturalHeight;
        if (r.width / r.height > ar) {
            drawH = r.height;
            drawW = r.height * ar;
        } else {
            drawW = r.width;
            drawH = r.width / ar;
        }
    }
    const maxX = Math.max(0, (drawW * zoomState.scale - r.width) / 2);
    const maxY = Math.max(0, (drawH * zoomState.scale - r.height) / 2);
    zoomState.tx = Math.min(maxX, Math.max(-maxX, zoomState.tx));
    zoomState.ty = Math.min(maxY, Math.max(-maxY, zoomState.ty));
}

// transform-origin is the frame's centre (the photo fills it edge to edge), so
// a source point q from that centre lands at t + scale*q and the pan limits
// above are symmetric.
function zoomApply(animate) {
    if (!zoomState.img) return;
    zoomState.img.style.transition = animate ? 'transform 0.2s ease-out' : 'none';
    zoomState.img.style.transform =
        `translate(${zoomState.tx}px, ${zoomState.ty}px) scale(${zoomState.scale})`;
}

// Zoom towards the tapped point, so whatever was under the finger stays put,
// and back to fit on the second double-tap.
function zoomToggleAt(x, y) {
    const frame = zoomFrame();
    if (!frame || !zoomState.img) return;
    const r = frame.getBoundingClientRect();
    const cx = r.left + r.width / 2;
    const cy = r.top + r.height / 2;
    if (zoomState.scale > 1) {
        zoomState.scale = 1;
        zoomState.tx = 0;
        zoomState.ty = 0;
    } else {
        const s = ZOOM_DOUBLE_TAP;
        zoomState.scale = s;
        zoomState.tx = (1 - s) * (x - cx);
        zoomState.ty = (1 - s) * (y - cy);
    }
    zoomClamp();
    zoomApply(true);
}

// A rotation moves the frame's centre out from under the transform.
window.addEventListener('orientationchange', zoomReset);

// Viewer gestures: pinch to zoom, drag to pan a zoomed photo, and swipe to
// change photo when at fit size. Touch only — a mouse uses the toolbar buttons.
(function initViewerGestures() {
    const content = document.getElementById('viewer-content');
    const MOVE_SLOP = 10;   // px of travel before a touch stops being a tap
    const SWIPE_MIN = 50;   // px of horizontal travel that changes photo
    const TAP_MS = 300;     // double-tap window

    let pinch = null;    // {d0, s0, tx0, ty0, m0}
    let single = null;   // {x0, y0, x, y, t0, moved, swipeable}
    let lastTap = 0;

    const frameCentre = () => {
        const r = content.getBoundingClientRect();
        return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
    };
    const pointOf = t => ({ x: t.clientX, y: t.clientY });
    const dist = (a, b) => Math.hypot(a.x - b.x, a.y - b.y);
    const mid = (a, b) => ({ x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 });

    content.addEventListener('touchstart', e => {
        if (e.touches.length >= 2) {
            // Two fingers are a pinch, never a swipe — whatever they end up doing.
            single = null;
            if (e.touches.length === 2 && zoomState.img) {
                const a = pointOf(e.touches[0]);
                const b = pointOf(e.touches[1]);
                pinch = {
                    d0: Math.max(1, dist(a, b)),
                    s0: zoomState.scale,
                    tx0: zoomState.tx,
                    ty0: zoomState.ty,
                    m0: mid(a, b),
                };
            }
            return;
        }
        if (e.touches.length !== 1) return;
        const t = pointOf(e.touches[0]);
        single = {
            x0: t.x, y0: t.y, x: t.x, y: t.y,
            t0: Date.now(), moved: false, swipeable: true,
        };
    }, { passive: false });

    content.addEventListener('touchmove', e => {
        if (pinch && e.touches.length >= 2) {
            e.preventDefault();
            const a = pointOf(e.touches[0]);
            const b = pointOf(e.touches[1]);
            const m1 = mid(a, b);
            const c = frameCentre();
            const s1 = Math.min(ZOOM_MAX, Math.max(1, pinch.s0 * dist(a, b) / pinch.d0));
            // Hold the photo point under the fingers still:
            //   t1 = m1 - c - s1 * (m0 - c - t0) / s0
            zoomState.scale = s1;
            zoomState.tx = m1.x - c.x - s1 * (pinch.m0.x - c.x - pinch.tx0) / pinch.s0;
            zoomState.ty = m1.y - c.y - s1 * (pinch.m0.y - c.y - pinch.ty0) / pinch.s0;
            zoomClamp();
            zoomApply(false);
            return;
        }
        if (!single || e.touches.length !== 1) return;
        const t = pointOf(e.touches[0]);
        if (Math.hypot(t.x - single.x0, t.y - single.y0) > MOVE_SLOP) single.moved = true;
        // A drag pans a zoomed photo. At fit size the same finger is a swipe,
        // but that is decided when it lifts, not here.
        if (zoomState.scale > 1) {
            e.preventDefault();
            zoomState.tx += t.x - single.x;
            zoomState.ty += t.y - single.y;
            single.x = t.x;
            single.y = t.y;
            single.x0 = t.x;
            single.y0 = t.y;
            zoomClamp();
            zoomApply(false);
        }
    }, { passive: false });

    content.addEventListener('touchend', e => {
        // One finger left after a pinch: carry on as a drag, never a swipe.
        if (pinch && e.touches.length === 1) {
            pinch = null;
            const t = pointOf(e.touches[0]);
            single = {
                x0: t.x, y0: t.y, x: t.x, y: t.y,
                t0: Date.now(), moved: true, swipeable: false,
            };
            zoomClamp();
            zoomApply(false);
            return;
        }
        if (e.touches.length > 0) return;   // a pinch is still in progress
        if (pinch) {
            pinch = null;
            zoomClamp();
            zoomApply(false);
            single = null;
            return;
        }
        if (!single) return;

        const s = single;
        single = null;
        const end = pointOf(e.changedTouches[0]);

        if (!s.moved && Date.now() - s.t0 < TAP_MS) {
            if (Date.now() - lastTap < TAP_MS) {
                lastTap = 0;
                zoomToggleAt(end.x, end.y);
                return;
            }
            lastTap = Date.now();
        } else {
            lastTap = 0;
        }

        if (!s.swipeable || zoomState.scale > 1) return;
        const dx = end.x - s.x0;
        const dy = end.y - s.y0;
        // Only act on clear horizontal swipes
        if (Math.abs(dx) > Math.abs(dy) && Math.abs(dx) > SWIPE_MIN) {
            if (dx < 0) {
                viewerNext();   // swipe left → next
            } else {
                viewerPrev();   // swipe right → previous
            }
        }
    }, { passive: false });

    content.addEventListener('touchcancel', () => {
        pinch = null;
        single = null;
        zoomClamp();
        zoomApply(false);
    }, { passive: true });
})();

// Event bindings
document.getElementById('viewer-close').addEventListener('click', closeViewer);
document.getElementById('viewer-fullscreen').addEventListener('click', toggleFullscreen);
document.getElementById('viewer-up').addEventListener('click', () => {
    closeViewer();
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

// Init — read any path from the fragment before `initAdmin` strips an admin one.
const initialPath = getPathFromHash() ?? '';
initAdmin();
initTheme();
loadAlbum(initialPath);
