// Download links: point every button at the matching file of the latest GitHub release and
// highlight the visitor's platform. Without JavaScript, or if GitHub's API can't be reached,
// the buttons keep their default link to the releases page.
(function () {
  "use strict";
  var REPO = "ZEraX4/FastFind";
  var LABELS = { windows: "Download for Windows", macos: "Download for macOS", linux: "Download for Linux" };
  var PRIMARY = { windows: "-setup.exe", macos: ".dmg", linux: ".AppImage" };

  function detectOs() {
    var ua = navigator.userAgent || "";
    var platform = ((navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || "").toLowerCase();
    if (/android|iphone|ipad|ipod/i.test(ua)) return null; // phones and tablets: show all options
    if (platform.indexOf("win") === 0 || /windows/i.test(ua)) return "windows";
    if (platform.indexOf("mac") === 0 || /mac os x/i.test(ua)) return "macos";
    if (platform.indexOf("linux") !== -1 || /linux|x11/i.test(ua)) return "linux";
    return null;
  }

  function size(bytes) {
    return bytes >= 1e6 ? Math.round(bytes / 1e6) + " MB" : Math.round(bytes / 1e3) + " KB";
  }

  function findAsset(assets, suffix) {
    for (var i = 0; i < assets.length; i++) {
      var name = assets[i].name;
      if (name.slice(-suffix.length) === suffix) return assets[i];
    }
    return null;
  }

  var os = detectOs();
  var hero = document.getElementById("hero-download");
  if (os) {
    document.getElementById("hero-label").textContent = LABELS[os];
    var card = document.querySelector('.dl[data-os="' + os + '"]');
    if (card) card.classList.add("detected");
  } else {
    hero.setAttribute("href", "#download");
  }

  fetch("https://api.github.com/repos/" + REPO + "/releases/latest", { headers: { Accept: "application/vnd.github+json" } })
    .then(function (r) {
      if (!r.ok) throw new Error("GitHub API " + r.status);
      return r.json();
    })
    .then(function (release) {
      var assets = (release.assets || []).filter(function (a) { return !/\.sig$/.test(a.name); });
      var version = String(release.tag_name || "").replace(/^v/, "");
      if (version) {
        document.getElementById("hero-version").textContent = "Version " + version + " · Free and open source";
        document.getElementById("dl-version").textContent = "Version " + version;
      }
      document.querySelectorAll("[data-asset]").forEach(function (link) {
        var asset = findAsset(assets, link.getAttribute("data-asset"));
        if (!asset) return;
        link.href = asset.browser_download_url;
        if (link.classList.contains("btn")) {
          var s = document.createElement("span");
          s.className = "size";
          s.textContent = " · " + size(asset.size);
          link.appendChild(s);
        }
      });
      if (os) {
        var primary = findAsset(assets, PRIMARY[os]);
        if (primary) hero.href = primary.browser_download_url;
      }
    })
    .catch(function () {
      /* keep the links to the releases page */
    });
})();
