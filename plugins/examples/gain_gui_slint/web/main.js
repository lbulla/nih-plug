document.addEventListener("DOMContentLoaded", (_) => {
    document.getElementById("cookie-checkbox").checked = getUseCookies();
});

function getUseCookies() {
    return document.cookie.indexOf("use-cookies=1") >= 0;
}

function setUseCookies(checkbox) {
    if (checkbox.checked) {
        document.cookie = "use-cookies=1; secure; SameSite=Strict";
    } else {
        const cookies = document.cookie.split(';');
        const date = new Date(0).toUTCString();
        for (let i = 0; i < cookies.length; i++) {
            document.cookie = cookies[i] + "=; expires="+ date;
        }

        document.cookie = "use-cookies=0; secure; SameSite=Strict";
    }
}

async function play() {
    const reader = new FileReader();
    reader.onload = async (e) => {
        window.config = e.target.result;
        await runModule();
    }

    const configFile = document.getElementById("config-file");
    if (configFile.files.length > 0) {
        reader.readAsText(configFile.files[0]);
    } else {
        await runModule();
    }
}

async function runModule() {
    document.getElementById("start-div").remove();
    document.getElementById("canvas-div").style.display = "block";

    const module = await import("./pkg/gain_gui_slint.js");
    await module.default();
}
