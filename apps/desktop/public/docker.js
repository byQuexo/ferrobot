// Shown by the desktop shell when the daemon reports no Docker engine; returns to the app once it does.
const HEALTH = "http://127.0.0.1:7878/v1/health";

async function check() {
  try {
    const response = await fetch(HEALTH);
    const health = await response.json();
    if (health.sandbox_ready) {
      location.replace("index.html");
      return;
    }
    document.getElementById("status").textContent = "Docker is still not reachable. Checking again…";
  } catch {
    document.getElementById("status").textContent = "The daemon is not answering. Checking again…";
  }
  setTimeout(check, 3000);
}

void check();
