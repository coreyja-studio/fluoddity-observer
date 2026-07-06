// Fluoddity field guide — behold overlay, lazy playback, HLS attach.

(function () {
  "use strict";

  // In CDN mode videos carry data-hls with an HLS playlist URL. Safari plays
  // it natively via src; everywhere else we attach hls.js.
  function attachHls(video) {
    var hlsUrl = video.dataset.hls;
    if (!hlsUrl) return;
    if (video.canPlayType("application/vnd.apple.mpegurl")) return;
    if (window.Hls && window.Hls.isSupported()) {
      var hls = new window.Hls({ capLevelToPlayerSize: true });
      hls.loadSource(hlsUrl);
      hls.attachMedia(video);
    }
  }

  document.querySelectorAll("video.specimen-video").forEach(attachHls);

  // Only play the loops that are actually on screen — a room holds a dozen
  // videos and phones shouldn't decode all of them at once.
  if ("IntersectionObserver" in window) {
    var observer = new IntersectionObserver(
      function (entries) {
        entries.forEach(function (entry) {
          var video = entry.target;
          if (entry.isIntersecting) {
            video.play().catch(function () {});
          } else {
            video.pause();
          }
        });
      },
      { rootMargin: "120px" }
    );
    document.querySelectorAll("video.specimen-video").forEach(function (v) {
      // Videos with native controls answer to the viewer, not the
      // observer — auto-resume would fight a deliberate pause.
      if (!v.controls) observer.observe(v);
    });
  }

  // Behold: click a specimen and the notebook falls away.
  var behold = document.getElementById("behold");
  if (!behold) return;
  var beholdVideo = behold.querySelector("video");
  var beholdImage = behold.querySelector("img");

  function showBehold() {
    behold.classList.add("open");
    behold.setAttribute("aria-hidden", "false");
    document.body.classList.add("beholding");
  }

  function openBehold(source) {
    beholdImage.removeAttribute("src");
    behold.classList.remove("still");
    // Full-bleed earns the archival copy: grids loop the CDN re-encode,
    // but behold trades up to the vault original when one exists.
    if (source.dataset.full) {
      delete beholdVideo.dataset.hls;
      beholdVideo.src = source.dataset.full;
    } else if (source.dataset.hls) {
      beholdVideo.removeAttribute("src");
      beholdVideo.dataset.hls = source.dataset.hls;
      if (beholdVideo.canPlayType("application/vnd.apple.mpegurl")) {
        beholdVideo.src = source.dataset.hls;
      } else {
        attachHls(beholdVideo);
      }
    } else {
      delete beholdVideo.dataset.hls;
      beholdVideo.src = source.getAttribute("src");
    }
    beholdVideo.poster = source.getAttribute("poster") || "";
    showBehold();
    beholdVideo.play().catch(function () {});
  }

  function openBeholdStill(source) {
    beholdVideo.pause();
    beholdVideo.removeAttribute("src");
    beholdVideo.load();
    behold.classList.add("still");
    beholdImage.src = source.getAttribute("src");
    beholdImage.alt = source.getAttribute("alt") || "";
    showBehold();
  }

  function closeBehold() {
    behold.classList.remove("open");
    behold.setAttribute("aria-hidden", "true");
    document.body.classList.remove("beholding");
    beholdVideo.pause();
    beholdVideo.removeAttribute("src");
    beholdVideo.load();
    beholdImage.removeAttribute("src");
  }

  document.querySelectorAll("video.specimen-video").forEach(function (video) {
    // Solo videos carry native controls — taps there pause and scrub;
    // the fullscreen button covers what behold does for grids.
    if (video.controls) return;
    video.addEventListener("click", function () {
      openBehold(video);
    });
  });

  document.querySelectorAll("img.specimen-image").forEach(function (img) {
    img.addEventListener("click", function () {
      openBeholdStill(img);
    });
  });

  behold.addEventListener("click", closeBehold);
  // The behold video has native controls now — pausing or scrubbing must
  // not fall through to the backdrop and close the overlay.
  beholdVideo.addEventListener("click", function (e) {
    e.stopPropagation();
  });
  document.addEventListener("keydown", function (e) {
    if (e.key === "Escape" && behold.classList.contains("open")) closeBehold();
  });
})();
