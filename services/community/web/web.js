// The web view's one script. Every page works without it; it adds the switch
// between light and dark (remembered in this browser), folds the channel list
// away on a narrow screen, reads a channel a few screens at a time from where it
// should open (its newest posts, a linked post, or the place kept for the visit)
// with "Jump to newest" and its own find (Ctrl+F), runs signing in with the
// Digital Credentials API, and shows each time in a post in the reader's own
// zone and way of writing dates.
(function () {
  'use strict'
  var KEY = 'wyvrn-web-theme'
  var root = document.documentElement
  function saved() {
    try { return localStorage.getItem(KEY) } catch (e) { return null }
  }
  // Before the page is drawn, so it doesn't flash the other way.
  var chosen = saved()
  if (chosen === 'light' || chosen === 'dark') root.setAttribute('data-theme', chosen)
  // A list of posts isn't shown until it is where it should be (postList), so it
  // isn't seen to jump; in case anything fails, shown anyway after a moment.
  root.classList.add('placing')
  setTimeout(function () { root.classList.remove('placing') }, 2500)

  function dark() {
    var set = root.getAttribute('data-theme')
    if (set) return set === 'dark'
    return window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches
  }

  document.addEventListener('DOMContentLoaded', function () {
    var button = document.querySelector('.theme-switch')
    if (button) {
      button.hidden = false
      button.setAttribute('aria-pressed', String(dark()))
      button.addEventListener('click', function () {
        var next = dark() ? 'light' : 'dark'
        root.setAttribute('data-theme', next)
        button.setAttribute('aria-pressed', String(next === 'dark'))
        try { localStorage.setItem(KEY, next) } catch (e) { /* not kept: fine */ }
      })
    }
    var side = document.querySelector('details.side')
    if (side && window.matchMedia && window.matchMedia('(max-width: 820px)').matches) side.open = false

    var signIn = document.getElementById('signin')
    if (signIn) setUpSignIn(signIn)

    showTimes(document)
    // Posts put in later (newer, older, a live change) have theirs shown too, and
    // "in 2 hours" stays true.
    if (window.MutationObserver) {
      new MutationObserver(function (changes) {
        changes.forEach(function (change) {
          Array.prototype.forEach.call(change.addedNodes, function (node) { if (node.querySelectorAll) showTimes(node) })
        })
      }).observe(document.body, { childList: true, subtree: true })
    }
    setInterval(function () { showTimes(document, true) }, 30000)

    var posts = document.querySelector('ol.posts')
    var list = posts ? postList(posts) : null
    var live = document.querySelector('[data-live]')
    if (list && live && window.EventSource) followLive(live.getAttribute('data-live'), list)
  })

  // A time in a post (render.rs's `<time class="timestamp">`, from `<t:…>`), as
  // the app shows one: in this browser's zone and language, `R` relative, the
  // whole of it on hover. `onlyRelative`: just those, which change as time passes.
  function showTimes(scope, onlyRelative) {
    var selector = onlyRelative ? 'time.timestamp[data-style="R"]' : 'time.timestamp'
    Array.prototype.forEach.call(scope.querySelectorAll(selector), function (time) {
      var when = new Date(time.getAttribute('datetime'))
      if (isNaN(when.getTime())) return
      var as = function (options) { return new Intl.DateTimeFormat(undefined, options).format(when) }
      var style = time.getAttribute('data-style')
      var shown
      if (style === 't') shown = as({ hour: 'numeric', minute: '2-digit' })
      else if (style === 'T') shown = as({ hour: 'numeric', minute: '2-digit', second: '2-digit' })
      else if (style === 'd') shown = as({ dateStyle: 'short' })
      else if (style === 'D') shown = as({ dateStyle: 'long' })
      else if (style === 'F') shown = as({ dateStyle: 'full', timeStyle: 'short' })
      else if (style === 'R') shown = relative(when)
      else shown = as({ dateStyle: 'long', timeStyle: 'short' })
      time.textContent = shown
      time.title = as({ dateStyle: 'full', timeStyle: 'short' })
    })
  }

  function relative(when) {
    var diff = Math.round((when.getTime() - Date.now()) / 1000)
    var units = [['year', 31536000], ['month', 2592000], ['week', 604800], ['day', 86400], ['hour', 3600], ['minute', 60], ['second', 1]]
    var format = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' })
    for (var i = 0; i < units.length; i++) {
      if (Math.abs(diff) >= units[i][1] || units[i][0] === 'second') return format.format(Math.round(diff / units[i][1]), units[i][0])
    }
  }

  // A channel's page, kept up to date as it is read: told when the channel moves
  // on (posting.rs's server-sent events), the list fetches its newest posts again
  // where it reaches them (postList's `refresh`). What is being written in the
  // form below is left alone.
  function followLive(url, list) {
    var heard = null
    var heardVoice = null
    var source = new EventSource(url)
    // The page again, from the server (not the browser's cache: a public page
    // may be kept a while); `then` is given it parsed.
    function fetchAgain(then) {
      fetch(location.pathname + location.search, { credentials: 'same-origin', cache: 'no-store' })
        .then(function (response) { return response.ok ? response.text() : null })
        .then(function (html) { if (html) then(new DOMParser().parseFromString(html, 'text/html')) })
        .catch(function () { /* the next change tries again */ })
    }
    source.addEventListener('channel', function (event) {
      if (heard === null) { heard = event.data; return }
      if (event.data === heard) return
      heard = event.data
      list.refresh()
    })
    // Who is in voice changed: the side's list of channels again.
    source.addEventListener('voice', function (event) {
      if (heardVoice === null) { heardVoice = event.data; return }
      if (event.data === heardVoice) return
      heardVoice = event.data
      var side = document.querySelector('nav[aria-label="Channels"]')
      if (!side) return
      fetchAgain(function (fresh) {
        var list = fresh.querySelector('nav[aria-label="Channels"]')
        if (list) side.innerHTML = list.innerHTML
      })
    })
  }

  // A list of posts read a screenful at a time. Every post the page has had is
  // kept (`entries`, oldest first), but only a run of them is in the page: about
  // two screens at first, never so many that the page is over four screens long
  // (the scrollbar's handle never under a quarter). As the reader nears either
  // end, more are put in, from what is kept or from the server's next page (the
  // "Older posts" and "Newer posts" links, which stay in the page for search
  // engines and for anyone without this script); those far off screen the other
  // way are taken out, and kept.
  //
  // A page opens where it should without being seen to move: at the newest posts,
  // at the post a link names (centred), or, coming back in this visit, where the
  // reader was (sessionStorage) -- placed before it is shown. "Jump to newest" is
  // there whenever the newest posts aren't on screen, and Ctrl+F (Cmd+F) is the
  // page's own find, over every post kept and then the server's search of the
  // channel.
  var SCREENS = 4
  var FIRST = 2
  function postList(posts) {
    var html = document.documentElement
    html.classList.add('post-list')
    var key = 'wyvrn-web-place:' + location.pathname + location.search
    var screen = function () { return window.innerHeight || 600 }
    var topBar = function () { var bar = document.querySelector('.top'); return bar ? bar.offsetHeight : 0 }
    var bottom = function () { return Math.max(0, html.scrollHeight - window.innerHeight) }
    var nearBottom = function () { return bottom() - window.scrollY < 48 }
    var seqOf = function (node) { var m = /^m(\d+)$/.exec(node.id || ''); return m ? +m[1] : null }
    var textOf = function (node) { var body = node.querySelector('.body') || node; return (body.textContent || '').replace(/\s+/g, ' ').trim() }
    var entry = function (node) { var text = textOf(node); return { id: node.id, seq: seqOf(node), node: node, text: text, lower: text.toLowerCase() } }

    var entries = Array.prototype.map.call(posts.querySelectorAll(':scope > li.post'), entry)
    var lo = 0
    var hi = entries.length
    var link = function (rel) { var a = document.querySelector('.paging a[rel="' + rel + '"]'); return a && a.getAttribute('href') ? a.href : null }
    // What lies beyond what is kept, each way: a page to fetch, or nothing.
    var olderHref = link('prev')
    var newerHref = link('next')
    var newestHref = posts.getAttribute('data-newest') ? new URL(posts.getAttribute('data-newest'), location.href).href : null
    var searchHref = posts.getAttribute('data-search') ? new URL(posts.getAttribute('data-search'), location.href).href : null
    // (Without IntersectionObserver the links stay, to be followed.)
    if (window.IntersectionObserver) Array.prototype.forEach.call(document.querySelectorAll('.paging'), function (p) { p.hidden = true })
    var edge = function (which) {
      var div = document.createElement('div')
      div.className = 'posts-edge ' + which
      div.setAttribute('aria-live', 'polite')
      return div
    }
    var topEdge = edge('older')
    var bottomEdge = edge('newer')
    posts.parentNode.insertBefore(topEdge, posts)
    posts.parentNode.insertBefore(bottomEdge, posts.nextSibling)
    var atNewest = function () { return hi === entries.length && !newerHref }

    // Where the reader is: the first post under the top bar, and how far it is
    // from it. Kept as they scroll, so that the page can be put back as it was
    // when posts come and go, or pictures grow, above it.
    var anchor = null
    function noteAnchor() {
      for (var i = lo; i < hi; i++) {
        var box = entries[i].node.getBoundingClientRect()
        if (box.bottom > topBar()) { anchor = { node: entries[i].node, top: box.top }; return }
      }
      anchor = null
    }
    function keepAnchor() {
      if (anchor && anchor.node.isConnected) window.scrollBy(0, anchor.node.getBoundingClientRect().top - anchor.top)
      noteAnchor()
    }
    // `change` done with what is on screen kept where it is.
    function anchored(change) {
      noteAnchor()
      change()
      keepAnchor()
    }
    var changing = false

    // Posts far off screen taken out, from the top or the bottom, while the page
    // is near its four screens; never one within a quarter of a screen of what is seen.
    function trim(fromTop) {
      anchored(function () {
        // (A little under four, for the pages' own head and foot and what comes next.)
        while (html.scrollHeight > (SCREENS - 0.5) * screen() && hi - lo > 1) {
          var node = (fromTop ? entries[lo] : entries[hi - 1]).node
          var box = node.getBoundingClientRect()
          if (fromTop ? box.bottom > -screen() / 4 : box.top < 1.25 * screen()) break
          node.remove()
          if (fromTop) lo++
          else hi--
        }
      })
    }

    // A page of posts (a paging link's), its posts made this page's own: their
    // addresses made whole, since it may be at another depth (a post's page loads
    // the channel's pages).
    function adopt(li, base) {
      var node = document.importNode(li, true)
      var all = [node].concat(Array.prototype.slice.call(node.querySelectorAll('[href],[src],[action],[poster]')))
      all.forEach(function (el) {
        ;['href', 'src', 'action', 'poster'].forEach(function (name) {
          var value = el.getAttribute(name)
          if (value === null || value.charAt(0) === '#') return
          try { el.setAttribute(name, new URL(value, base).href) } catch (e) { /* left as it was */ }
        })
      })
      return node
    }
    function fetchPage(address, where, cache) {
      if (where) where.textContent = where === topEdge ? 'Loading older posts…' : 'Loading newer posts…'
      return fetch(address, { credentials: 'same-origin', cache: cache || 'default' })
        .then(function (r) { if (!r.ok) throw new Error('HTTP ' + r.status); return r.text() })
        .then(function (text) {
          var page = new DOMParser().parseFromString(text, 'text/html')
          var rel = function (name) { var a = page.querySelector('.paging a[rel="' + name + '"]'); return a && a.getAttribute('href') ? new URL(a.getAttribute('href'), address).href : null }
          if (where) where.textContent = ''
          return {
            items: Array.prototype.map.call(page.querySelectorAll('ol.posts > li.post'), function (li) { return entry(adopt(li, address)) }),
            prev: rel('prev'),
            next: rel('next'),
          }
        })
        .catch(function () {
          if (where) where.textContent = 'Couldn’t load more posts. They’re tried again as you scroll.'
          return null
        })
    }
    var held = function (id) { for (var i = 0; i < entries.length; i++) if (entries[i].id === id) return i; return -1 }

    // About a screen more, older or newer: from what is kept, or the server.
    var loading = false
    function more(older) {
      if (loading) return Promise.resolve(false)
      if (older ? lo > 0 : hi < entries.length) {
        changing = true
        anchored(function () {
          var added = 0
          while ((older ? lo > 0 : hi < entries.length) && added < 0.75 * screen()) {
            var e = older ? entries[--lo] : entries[hi++]
            if (older) posts.insertBefore(e.node, posts.firstChild)
            else posts.appendChild(e.node)
            added += e.node.offsetHeight
          }
        })
        trim(!older)
        changing = false
        return Promise.resolve(true)
      }
      var address = older ? olderHref : newerHref
      if (!address) return Promise.resolve(false)
      loading = true
      return fetchPage(address, older ? topEdge : bottomEdge).then(function (got) {
        loading = false
        if (!got) return false
        var fresh = got.items.filter(function (e) { return held(e.id) < 0 })
        if (older) {
          olderHref = got.prev && got.prev !== address ? got.prev : null
          entries = fresh.concat(entries)
          lo += fresh.length
          hi += fresh.length
        } else {
          newerHref = got.next && got.next !== address ? got.next : null
          entries = entries.concat(fresh)
        }
        // (A page of nothing new goes on to the next one.)
        return more(older)
      })
    }

    // Only what is around `index` in the page, it shown as `how` says: centred,
    // at the bottom, or at a kept place (`{ into }`).
    function windowTo(index, how) {
      changing = true
      for (var i = lo; i < hi; i++) entries[i].node.remove()
      lo = index
      hi = index + 1
      posts.appendChild(entries[index].node)
      var height = function () { return posts.offsetHeight }
      var turn = how === 'bottom'
      while (height() < FIRST * screen() && (lo > 0 || hi < entries.length)) {
        if ((turn || hi >= entries.length) && lo > 0) posts.insertBefore(entries[--lo].node, posts.firstChild)
        else if (hi < entries.length) posts.appendChild(entries[hi++].node)
        turn = !turn
      }
      var node = entries[index].node
      if (how === 'bottom') window.scrollTo(0, bottom())
      else if (how && typeof how.into === 'number') window.scrollTo(0, window.scrollY + node.getBoundingClientRect().top - topBar() + how.into)
      else window.scrollTo(0, window.scrollY + node.getBoundingClientRect().top - (window.innerHeight - node.offsetHeight) / 2)
      noteAnchor()
      changing = false
    }

    // The ends: nearing one, more comes.
    var watching = window.IntersectionObserver
      ? new IntersectionObserver(function (seen) {
          seen.forEach(function (entry) {
            if (!entry.isIntersecting || placing) return
            var older = entry.target === topEdge
            more(older).then(function (any) {
              // Still in sight (a short page): on again, once it has settled.
              if (any) { watching.unobserve(entry.target); setTimeout(function () { watching.observe(entry.target) }, 120) }
              showJump()
            })
          })
        }, { rootMargin: '100% 0px' })
      : null

    // Jump to newest.
    var jump = document.createElement('button')
    jump.type = 'button'
    jump.className = 'button jump-newest'
    jump.textContent = 'Jump to newest ↓'
    jump.hidden = true
    document.body.appendChild(jump)
    function showJump() {
      jump.hidden = atNewest() && bottom() - window.scrollY < screen()
    }
    jump.addEventListener('click', function () {
      if (!newerHref) {
        windowTo(entries.length - 1, 'bottom')
        pinned = true
        showJump()
      } else if (newestHref) {
        var there = new URL(newestHref)
        try { sessionStorage.setItem('wyvrn-web-place:' + there.pathname + there.search, JSON.stringify({ bottom: true })) } catch (e) { /* fine */ }
        location.assign(there.href)
      }
    })

    // Kept for this visit: the post at the top and how far into it, or the bottom.
    function read() {
      try { return JSON.parse(sessionStorage.getItem(key) || 'null') } catch (e) { return null }
    }
    function save() {
      var place = { bottom: true }
      if (!(atNewest() && nearBottom())) {
        noteAnchor()
        if (anchor) place = { post: anchor.node.id, seq: seqOf(anchor.node), into: topBar() - anchor.top }
      }
      try { sessionStorage.setItem(key, JSON.stringify(place)) } catch (e) { /* not kept: fine */ }
    }

    // Placing the page, unseen.
    var placing = true
    var pinned = false
    var moved = false
    function shown() {
      placing = false
      html.classList.remove('placing')
      pinned = atNewest() && nearBottom()
      if (watching) { watching.observe(topEdge); watching.observe(bottomEdge) }
      showJump()
    }
    function place() {
      if (!entries.length) return Promise.resolve()
      var focus = posts.querySelector('li.post.focus')
      var hashed = location.hash && location.hash.length > 1 ? document.getElementById(decodeURIComponent(location.hash.slice(1))) : null
      if (focus || (hashed && hashed.matches('li.post'))) { windowTo(held((focus || hashed).id), 'center'); return Promise.resolve() }
      if (hashed) { windowTo(entries.length - 1, 'bottom'); hashed.scrollIntoView(); return Promise.resolve() }
      var saved = read()
      if (saved && saved.post && held(saved.post) >= 0) { windowTo(held(saved.post), { into: saved.into || 0 }); return Promise.resolve() }
      // Further back than this page: the posts around it fetched in its place.
      if (saved && saved.seq && newestHref && !newerHref && entries[0] && saved.seq < entries[0].seq) {
        var address = new URL(newestHref)
        address.searchParams.set('after', String(Math.max(0, saved.seq - 12)))
        return fetchPage(address.href, null).then(function (got) {
          var at = got ? got.items.map(function (e) { return e.id }).indexOf(saved.post) : -1
          if (at < 0) return windowTo(entries.length - 1, 'bottom')
          for (var i = lo; i < hi; i++) entries[i].node.remove()
          entries = got.items
          lo = hi = 0
          olderHref = got.prev
          newerHref = got.next
          windowTo(at, { into: saved.into || 0 })
        })
      }
      windowTo(entries.length - 1, 'bottom')
      return Promise.resolve()
    }
    if ('scrollRestoration' in history) history.scrollRestoration = 'manual'
    place().then(function () { requestAnimationFrame(shown) })

    // As pictures arrive and grow the posts: at the newest, kept at the bottom, as
    // a chat is; anywhere else, the post read kept where it is.
    ;['wheel', 'touchstart', 'keydown', 'mousedown'].forEach(function (name) {
      window.addEventListener(name, function () { moved = true }, { passive: true })
    })
    var pending = null
    window.addEventListener('scroll', function () {
      if (changing || placing) return
      if (moved) pinned = atNewest() && nearBottom()
      noteAnchor()
      showJump()
      if (pending) return
      pending = setTimeout(function () { pending = null; save() }, 200)
    }, { passive: true })
    window.addEventListener('pagehide', save)
    function hold() {
      // (Not while posts are being put in or out: that keeps its own place. A page
      // being fetched changes nothing until it is here.)
      if (changing) return
      if (pinned) window.scrollTo(0, bottom())
      else keepAnchor()
    }
    if (window.ResizeObserver) new ResizeObserver(hold).observe(posts)
    window.addEventListener('load', hold)

    // The channel moved on (posting.rs's live events): where what is kept reaches
    // the newest, its newest page again, its posts put in place of those kept (an
    // edit, a reaction), new ones after them, and those gone taken out.
    function refresh() {
      if (newerHref || !newestHref) return
      fetchPage(newestHref, null, 'no-store').then(function (got) {
        if (!got || !got.items.length) return
        var first = got.items[0].seq
        var wasPinned = pinned
        changing = true
        anchored(function () {
          var ids = {}
          got.items.forEach(function (fresh) {
            ids[fresh.id] = true
            var at = held(fresh.id)
            if (at >= 0) {
              if (at >= lo && at < hi) entries[at].node.replaceWith(fresh.node)
              entries[at] = fresh
            } else if (!entries.length || fresh.seq > entries[entries.length - 1].seq) {
              var showing = hi === entries.length
              entries.push(fresh)
              if (showing) { posts.appendChild(fresh.node); hi++ }
            }
          })
          for (var i = entries.length - 1; i >= 0; i--) {
            var e = entries[i]
            if (e.seq === null || e.seq < first || ids[e.id]) continue
            if (i >= lo && i < hi) { e.node.remove(); hi-- }
            else if (i < lo) { lo--; hi-- }
            entries.splice(i, 1)
          }
        })
        changing = false
        if (wasPinned) window.scrollTo(0, bottom())
        trim(true)
        if (finding) finding.again()
        showJump()
      })
    }

    // The page's own find.
    var finding = setUpFind({
      entries: function () { return entries },
      shown: function (i) { return i >= lo && i < hi },
      showAt: function (i) {
        if (i >= lo && i < hi) {
          var node = entries[i].node
          window.scrollTo(0, window.scrollY + node.getBoundingClientRect().top - (window.innerHeight - node.offsetHeight) / 2)
          noteAnchor()
        } else windowTo(i, 'center')
        pinned = false
        showJump()
      },
      search: searchHref,
      held: held,
    })

    return { refresh: refresh }
  }

  // Ctrl+F (Cmd+F): a find bar over the posts the page has had, kept or shown, and
  // then the server's search of this channel. Pressed again in the bar, the
  // browser's own find; Escape closes it.
  function setUpFind(list) {
    var bar = document.createElement('div')
    bar.className = 'find'
    bar.setAttribute('role', 'search')
    bar.hidden = true
    bar.innerHTML =
      '<div class="find-row">' +
      '<input type="search" class="find-input" aria-label="Find in these posts" placeholder="Find in these posts">' +
      '<span class="find-count" aria-live="polite"></span>' +
      '<button type="button" class="button quiet icon find-step" data-step="-1" aria-label="Previous" title="Previous (Shift+Enter)">↑</button>' +
      '<button type="button" class="button quiet icon find-step" data-step="1" aria-label="Next" title="Next (Enter)">↓</button>' +
      '<button type="button" class="button quiet icon find-close" aria-label="Close" title="Close (Escape)">✕</button>' +
      (list.search ? '<button type="button" class="button quiet find-server">Search the whole channel</button>' : '') +
      '</div>' +
      '<ol class="find-results" hidden></ol>'
    document.body.appendChild(bar)
    var input = bar.querySelector('.find-input')
    var count = bar.querySelector('.find-count')
    var results = bar.querySelector('.find-results')
    var serverButton = bar.querySelector('.find-server')
    var matches = []
    var current = -1
    var asked = null
    var highlights = window.CSS && CSS.highlights && window.Highlight
    var words = function () { return input.value.toLowerCase().split(/\s+/).filter(Boolean) }

    function paint() {
      Array.prototype.forEach.call(document.querySelectorAll('li.post.found'), function (li) { li.classList.remove('found') })
      if (highlights) { CSS.highlights.delete('wyvrn-find'); CSS.highlights.delete('wyvrn-find-current') }
      var all = list.entries()
      var wanted = words()
      if (!wanted.length) return
      var ranges = []
      var currentRanges = []
      matches.forEach(function (index, n) {
        if (!list.shown(index)) return
        var e = all[index]
        if (n === current) e.node.classList.add('found')
        if (!highlights) return
        var walker = document.createTreeWalker(e.node.querySelector('.body') || e.node, NodeFilter.SHOW_TEXT)
        for (var text = walker.nextNode(); text; text = walker.nextNode()) {
          var lower = text.data.toLowerCase()
          wanted.forEach(function (word) {
            for (var at = lower.indexOf(word); at >= 0; at = lower.indexOf(word, at + word.length)) {
              var range = new Range()
              range.setStart(text, at)
              range.setEnd(text, at + word.length)
              ;(n === current ? currentRanges : ranges).push(range)
            }
          })
        }
      })
      CSS.highlights.set('wyvrn-find', new Highlight(...ranges))
      CSS.highlights.set('wyvrn-find-current', new Highlight(...currentRanges))
    }
    function say() {
      if (!words().length) count.textContent = ''
      else if (!matches.length) count.textContent = 'None here'
      else count.textContent = (current + 1) + ' of ' + matches.length
    }
    function find(keep) {
      var wanted = words()
      var all = list.entries()
      var was = current >= 0 ? matches[current] : null
      matches = []
      if (wanted.length) all.forEach(function (e, i) { if (wanted.every(function (w) { return e.lower.indexOf(w) >= 0 })) matches.push(i) })
      current = -1
      if (matches.length) {
        if (keep && was !== null && matches.indexOf(was) >= 0) current = matches.indexOf(was)
        else {
          // The newest match at or above what is on screen, as a chat's find starts.
          current = matches.length - 1
          for (var n = matches.length - 1; n >= 0; n--) {
            if (list.shown(matches[n]) && all[matches[n]].node.getBoundingClientRect().top < window.innerHeight) { current = n; break }
          }
        }
      }
      say()
      if (!keep && current >= 0) list.showAt(matches[current])
      paint()
      results.hidden = true
      if (!keep && wanted.join(' ').length >= 2 && !matches.length && list.search) askSoon()
    }
    function step(by) {
      if (!matches.length) return
      current = (current + by + matches.length) % matches.length
      list.showAt(matches[current])
      say()
      paint()
    }
    var timer = null
    function askSoon() {
      clearTimeout(timer)
      timer = setTimeout(askServer, 400)
    }
    function askServer() {
      var query = input.value.trim()
      if (!list.search || query.length < 2) return
      asked = query
      var address = new URL(list.search)
      address.searchParams.set('q', query)
      results.hidden = false
      results.innerHTML = '<li class="quiet">Searching the channel…</li>'
      fetch(address.href, { credentials: 'same-origin' })
        .then(function (r) { if (!r.ok) throw new Error('HTTP ' + r.status); return r.text() })
        .then(function (text) {
          if (asked !== query) return
          var page = new DOMParser().parseFromString(text, 'text/html')
          var found = page.querySelectorAll('ol.posts > li.post')
          results.innerHTML = ''
          if (!found.length) { results.innerHTML = '<li class="quiet">Nothing in the channel says that.</li>'; return }
          Array.prototype.forEach.call(found, function (li) {
            var time = li.querySelector('a.time')
            var href = time ? new URL(time.getAttribute('href'), address.href).href : null
            var item = document.createElement('li')
            var go = document.createElement('a')
            go.className = 'find-result'
            if (href) go.href = href
            var who = document.createElement('span')
            who.className = 'author'
            who.textContent = (li.querySelector('.author') || {}).textContent || ''
            var when = document.createElement('span')
            when.className = 'quiet'
            when.textContent = ' ' + ((li.querySelector('time') || {}).textContent || '')
            var said = document.createElement('span')
            said.className = 'find-result-text'
            said.textContent = ((li.querySelector('.content') || li).textContent || '').replace(/\s+/g, ' ').trim().slice(0, 160)
            go.appendChild(who)
            go.appendChild(when)
            go.appendChild(said)
            // One this page has: gone to here, not loaded again.
            var at = list.held(li.id)
            if (at >= 0) go.addEventListener('click', function (event) { event.preventDefault(); list.showAt(at) })
            item.appendChild(go)
            results.appendChild(item)
          })
        })
        .catch(function () { if (asked === query) results.innerHTML = '<li class="quiet">The search didn’t answer. Try again.</li>' })
    }
    function open() {
      bar.hidden = false
      input.focus()
      input.select()
    }
    function close() {
      bar.hidden = true
      results.hidden = true
      matches = []
      current = -1
      paint()
    }
    input.addEventListener('input', function () { find(false) })
    input.addEventListener('keydown', function (event) {
      if (event.key === 'Enter') { event.preventDefault(); if (matches.length) step(event.shiftKey ? -1 : 1); else askServer() }
      else if (event.key === 'Escape') { event.preventDefault(); close() }
    })
    Array.prototype.forEach.call(bar.querySelectorAll('.find-step'), function (button) {
      button.addEventListener('click', function () { step(+button.getAttribute('data-step')) })
    })
    if (serverButton) serverButton.addEventListener('click', askServer)
    bar.querySelector('.find-close').addEventListener('click', close)
    document.addEventListener('keydown', function (event) {
      var f = (event.key === 'f' || event.key === 'F') && (event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey
      if (f) {
        // Again, in the bar: the browser's own find.
        if (!bar.hidden && document.activeElement === input) return
        event.preventDefault()
        open()
      } else if (event.key === 'Escape' && !bar.hidden) close()
    })
    return {
      // What is kept changed: the matches again, staying on the one shown.
      again: function () { if (!bar.hidden && words().length) find(true) },
    }
  }

  function setUpSignIn(button) {
    var status = document.getElementById('status')
    var base = button.getAttribute('data-base') || './'
    var then = button.getAttribute('data-then') || './'
    function say(text, bad) {
      status.textContent = text
      status.className = bad ? 'status bad' : 'status'
    }
    if (typeof window.DigitalCredential === 'undefined' || !navigator.credentials) {
      button.disabled = true
      say('This browser can’t ask your phone for Wyvrn’s sign-in. Use Chrome on Android, or Chrome on a computer with your phone nearby.', true)
      return
    }
    button.addEventListener('click', function () {
      button.disabled = true
      say('Asking…')
      fetch(base + 'sign-in/request', { credentials: 'same-origin' })
        .then(function (r) { return r.json() })
        .then(function (asked) {
          if (asked.error) throw new Error(asked.error)
          say('Choose Wyvrn on your phone.')
          return navigator.credentials.get({ digital: { requests: [{ protocol: asked.request.protocol, data: asked.request.data }] } }).then(function (answer) {
            say('Checking… your phone answers once more.')
            return fetch(base + 'sign-in/response', {
              method: 'POST',
              credentials: 'same-origin',
              headers: { 'content-type': 'application/json' },
              body: JSON.stringify({ id: asked.id, data: answer.data }),
            })
          })
        })
        .then(function (r) {
          return r.json().then(function (body) {
            if (!r.ok) throw new Error(body.error || 'HTTP ' + r.status)
            say('Signed in.')
            window.location.assign(then)
          })
        })
        .catch(function (e) {
          say(e && (e.name === 'NotAllowedError' || e.name === 'AbortError') ? 'Cancelled.' : (e && e.message) || 'That didn’t work.', true)
        })
        .then(function () { button.disabled = false })
    })
  }
})()
