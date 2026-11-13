window.orqella.ready(() => {
  const quote = document.getElementById("quote");
  const error = document.getElementById("error");
  document.getElementById("go").onclick = async () => {
    error.textContent = "";
    try {
      const result = await window.orqella.call("backend.request", { method: "GET", path: "/api/quote" });
      quote.textContent = `“${result.quote}” — ${result.machine}`;
    } catch (e) { error.textContent = e.message; }
  };
});
