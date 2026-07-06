// Ambient mode: the archive as a slow, endless exhibition. Two stacked
// layers crossfade; a video plays its loop a couple of times, a still
// holds like a projector slide, the label breathes in and out, and the
// site disappears entirely.

(function () {
  "use strict";

  var MIN_DWELL_MS = 14000; // replay short loops until at least this long
  var MAX_DWELL_MS = 45000; // hard cap for long clips or stalled playback
  var STILL_DWELL_MS = 12000; // how long a projector slide holds
  var FADE_MS = 1600;

  var data = document.getElementById("ambient-data");
  if (!data) return;
  var playlist = JSON.parse(data.textContent);
  if (!playlist.length) return;

  // Fisher–Yates; a fresh walk through the collection every visit.
  for (var i = playlist.length - 1; i > 0; i--) {
    var j = Math.floor(Math.random() * (i + 1));
    var tmp = playlist[i];
    playlist[i] = playlist[j];
    playlist[j] = tmp;
  }

  var stage = document.getElementById("ambient-stage");
  var label = document.getElementById("ambient-label");
  var layers = stage.querySelectorAll(".layer");
  var front = 0; // index into `layers` of the visible layer
  var cursor = 0; // index into `playlist`

  function attachHls(video, hlsUrl) {
    if (!hlsUrl) return;
    if (video.canPlayType("application/vnd.apple.mpegurl")) return;
    if (window.Hls && window.Hls.isSupported()) {
      var hls = new window.Hls({ capLevelToPlayerSize: true });
      hls.loadSource(hlsUrl);
      hls.attachMedia(video);
      video._hls = hls;
    }
  }

  function load(video, entry) {
    if (video._hls) {
      video._hls.destroy();
      delete video._hls;
    }
    video.poster = entry.poster || "";
    if (entry.hls && !video.canPlayType("application/vnd.apple.mpegurl")) {
      video.removeAttribute("src");
      attachHls(video, entry.hls);
    } else {
      video.src = entry.src;
    }
    video.load();
  }

  function reveal(incomingLayer, outgoingLayer, entry) {
    incomingLayer.classList.add("visible");
    outgoingLayer.classList.remove("visible");
    front = 1 - front;
    label.textContent = entry.label;
    label.classList.remove("breathe");
    void label.offsetWidth; // restart the animation
    label.classList.add("breathe");
    window.setTimeout(function () {
      outgoingLayer.querySelector("video").pause();
    }, FADE_MS);
  }

  function next() {
    var entry = playlist[cursor % playlist.length];
    cursor += 1;
    var incomingLayer = layers[1 - front];
    var outgoingLayer = layers[front];
    var incoming = incomingLayer.querySelector("video");
    var still = incomingLayer.querySelector("img");

    var advanced = false;
    function advance() {
      if (advanced) return;
      advanced = true;
      incoming.removeEventListener("ended", onEnded);
      next();
    }

    if (entry.kind === "image") {
      // A projector slide: hold, then move along.
      incomingLayer.classList.add("still");
      incoming.removeAttribute("src");
      still.src = entry.src;
      var shown = false;
      function showStill() {
        if (shown) return;
        shown = true;
        reveal(incomingLayer, outgoingLayer, entry);
        window.setTimeout(advance, STILL_DWELL_MS);
      }
      still.onload = showStill;
      // Broken image or cache race — don't stall the exhibition.
      still.onerror = function () {
        window.setTimeout(advance, 1500);
      };
      if (still.complete && still.naturalWidth > 0) showStill();
      return;
    }

    incomingLayer.classList.remove("still");
    still.removeAttribute("src");
    load(incoming, entry);

    var started = null;
    function onEnded() {
      var elapsed = started ? Date.now() - started : MAX_DWELL_MS;
      if (elapsed >= MIN_DWELL_MS) {
        advance();
      } else {
        incoming.play().catch(function () {});
      }
    }

    incoming
      .play()
      .then(function () {
        started = Date.now();
        // Reveal only once playback is truly rolling.
        reveal(incomingLayer, outgoingLayer, entry);
        window.setTimeout(advance, MAX_DWELL_MS);
      })
      .catch(function () {
        // Undecodable or blocked — move along after a beat.
        window.setTimeout(advance, 1500);
      });
    incoming.addEventListener("ended", onEnded);
  }

  // Esc or click leaves the exhibition.
  function exit() {
    if (document.referrer && new URL(document.referrer).origin === location.origin) {
      history.back();
    } else {
      location.href = "/";
    }
  }
  document.addEventListener("keydown", function (e) {
    if (e.key === "Escape") exit();
  });
  document.getElementById("ambient-exit").addEventListener("click", exit);
  // Advance on demand: space or right arrow.
  document.addEventListener("keydown", function (e) {
    if (e.key === " " || e.key === "ArrowRight") {
      e.preventDefault();
      next();
    }
  });

  next();
})();
