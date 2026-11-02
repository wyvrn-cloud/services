// The web view's one script. Every page works without it; it adds the switch
// between light and dark (remembered in this browser), folds the channel list
// away on a narrow screen, and runs signing in with the Digital Credentials API.
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
  })

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
