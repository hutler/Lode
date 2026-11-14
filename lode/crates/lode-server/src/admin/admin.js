// Lode admin web UI. Pages work without this; it adds confirmations, live refresh and copy
// buttons. Served as a file: the content security policy allows no inline script.
"use strict";
document.addEventListener("submit", function (e) {
  var form = e.target;
  var text = form.getAttribute("data-confirm");
  if (text && !window.confirm(text)) {
    e.preventDefault();
  }
});
document.addEventListener("click", function (e) {
  var button = e.target.closest ? e.target.closest("[data-copy]") : null;
  if (!button) return;
  var source = document.getElementById(button.getAttribute("data-copy"));
  if (!source || !navigator.clipboard) return;
  navigator.clipboard.writeText(source.textContent).then(function () {
    button.textContent = "Copied";
  });
});
(function () {
  var seconds = parseInt(document.body.getAttribute("data-refresh") || "0", 10);
  if (!seconds) return;
  setInterval(function () {
    var active = document.activeElement;
    if (document.hidden || (active && /^(INPUT|TEXTAREA|SELECT)$/.test(active.tagName))) return;
    fetch(window.location.href, { credentials: "same-origin" })
      .then(function (r) { return r.ok && !r.redirected ? r.text() : null; })
      .then(function (text) {
        if (!text) return;
        var doc = new DOMParser().parseFromString(text, "text/html");
        var fresh = doc.getElementById("main");
        var main = document.getElementById("main");
        if (fresh && main) main.innerHTML = fresh.innerHTML;
      })
      .catch(function () {});
  }, seconds * 1000);
})();
