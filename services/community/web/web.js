// The web view's one script. Every page works without it; it adds the switch
// between light and dark (remembered in this browser), folds the channel list
// away on a narrow screen, keeps each channel's place for the visit (starting
// at its newest posts), and runs signing in with the Digital Credentials API.
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

    var posts = document.querySelector('ol.posts')
    if (posts) keepPlace(posts)
  })

  // Where a list of posts was left, for this visit (sessionStorage), by page: the
  // post at the top of the screen and how far into it, or "at the bottom". Kept by
  // post rather than by distance so that new posts don't move it. A page seen for
  // the first time starts at the bottom, with the newest posts; a link to a post
  // goes to that post.
  function keepPlace(posts) {
    var key = 'wyvrn-web-place:' + location.pathname + location.search
    var top = function () { var bar = document.querySelector('.top'); return bar ? bar.offsetHeight : 0 }
    var bottom = function () { return Math.max(0, document.documentElement.scrollHeight - window.innerHeight) }
    var atBottom = function () { return bottom() - window.scrollY < 48 }
    function read() {
      try { return JSON.parse(sessionStorage.getItem(key) || 'null') } catch (e) { return null }
    }
    function save() {
      var place = { bottom: true }
      if (!atBottom()) {
        var items = posts.querySelectorAll('li.post')
        for (var i = 0; i < items.length; i++) {
          var box = items[i].getBoundingClientRect()
          if (box.bottom > top()) { place = { post: items[i].id, into: top() - box.top }; break }
        }
      }
      try { sessionStorage.setItem(key, JSON.stringify(place)) } catch (e) { /* not kept: fine */ }
    }
    // Where to be, worked out again as pictures load and move things.
    function go() {
      var focus = posts.querySelector('li.post.focus')
      if (focus) return focus.scrollIntoView({ block: 'center' })
      var place = read()
      var post = place && place.post && document.getElementById(place.post)
      if (post) window.scrollTo(0, window.scrollY + post.getBoundingClientRect().top - top() + (place.into || 0))
      else window.scrollTo(0, bottom())
    }
    // A link to somewhere on the page is the browser's to follow.
    if (location.hash) return listen()
    if ('scrollRestoration' in history) history.scrollRestoration = 'manual'
    go()
    // Pictures arriving (a pack's file fetched for the first time can take a few
    // seconds) grow the posts. At the bottom, the page stays at the bottom, as a chat
    // does, for as long as it is open; at a place come back to, it stays on that post
    // until the reader moves.
    var pinned = atBottom()
    var moved = false
    ;['wheel', 'touchstart', 'keydown', 'mousedown'].forEach(function (name) {
      window.addEventListener(name, function () { moved = true }, { passive: true })
    })
    window.addEventListener('scroll', function () { if (moved) pinned = atBottom() }, { passive: true })
    function hold() {
      if (pinned) window.scrollTo(0, bottom())
      else if (!moved) go()
    }
    if (window.ResizeObserver) new ResizeObserver(hold).observe(posts)
    window.addEventListener('load', hold)
    listen()

    function listen() {
      var pending = null
      window.addEventListener('scroll', function () {
        if (pending) return
        pending = setTimeout(function () { pending = null; save() }, 200)
      }, { passive: true })
      window.addEventListener('pagehide', save)
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
