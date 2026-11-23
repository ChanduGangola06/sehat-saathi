"""Real-browser test (Playwright + Chrome) of the chat UI with daily token limits.
Run through dev/run_browser_test.sh, which starts the fake OpenAI, fake Clerk, the backend (free plan = 1,000 tokens/day)
and a test build of the frontend.

Clerk's own sign-in screens cannot be exercised offline (they load from Clerk's servers), so this test opens the
test-only /e2e page with a Clerk-style token for a fake user. Everything after sign-in is the real UI and the real API."""
import os, re, subprocess, sys
from playwright.sync_api import sync_playwright, expect

APP = "http://localhost:3000"
uid = "user_browser_" + os.urandom(3).hex()
token = subprocess.check_output([sys.executable, os.path.join(os.path.dirname(__file__), "mock_clerk.py"), "token", uid], text=True).strip()
ok = True
def check(name, cond):
    global ok
    print(("  ok   " if cond else "  FAIL ") + name)
    ok = ok and bool(cond)

with sync_playwright() as p:
    browser = p.chromium.launch(executable_path="/usr/bin/google-chrome", headless=True, args=["--no-sandbox"])
    ctx = browser.new_context(viewport={"width": 1280, "height": 800}, timezone_id="Asia/Kolkata", locale="en-IN")
    page = ctx.new_page()
    errors = []
    page.on("pageerror", lambda e: errors.append(str(e)))
    # Clerk's script would be fetched from a fake domain; fail fast instead of waiting.
    page.route(re.compile(r"clerk\.accounts\.dev|clerk\.mock"), lambda r: r.abort())
    page.goto(f"{APP}/e2e?t={token}")

    box = page.get_by_placeholder("Type a message…")
    expect(box).to_be_visible()
    check("chat UI loads for a signed-in user", True)

    expect(page.get_by_test_id("usage")).to_contain_text("1,000 / 1,000 tokens left today")
    check("free plan shows a DAILY allowance (1,000)", True)
    expect(page.get_by_test_id("resets")).to_contain_text(re.compile(r"Asia/(Kolkata|Calcutta)"))
    expect(page.get_by_test_id("resets")).to_contain_text(re.compile(r"Resets at 12:00\s?am", re.I))
    check("shows the reset time: midnight in the user's own timezone", True)

    stored = page.evaluate("JSON.stringify([localStorage, sessionStorage])")
    check("no tokens kept in localStorage/sessionStorage", "eyJ" not in stored)
    check("no cookie is set by our API", not [c for c in ctx.cookies() if c["name"].startswith("Sehat Saathi_rt")])

    # One chat turn with a tool call costs more than 1,000 tokens in the mock (600 per model call, 2 calls)
    box.fill("my BP is 130/85"); box.press("Enter")
    expect(page.get_by_text("[mock final]", exact=False)).to_be_visible(timeout=15000)
    check("agent replied", True)
    expect(page.get_by_text("130/85").first).to_be_visible()
    check("vital appears in health log", True)
    expect(page.get_by_test_id("usage")).to_contain_text("0 / 1,000 tokens left today")
    check("one message used up the whole 1,000 daily tokens", True)
    box.fill("please remind me"); box.press("Enter")
    expect(page.locator(".err")).to_contain_text("come back at midnight", timeout=15000)
    check("out-of-tokens message explains the midnight reset", True)

    box.fill("I have severe chest pain"); box.press("Enter")
    expect(page.locator(".msg.emergency")).to_be_visible(timeout=15000)
    check("emergency is still answered at 0 tokens", True)

    page.locator(".usage").get_by_role("button", name="Upgrade").click()
    expect(page.get_by_role("heading", name="Plans")).to_be_visible()
    expect(page.locator(".plan", has_text="Plus")).to_contain_text("20,000 tokens per day")
    page.locator(".plan", has_text="Plus").get_by_role("button").click()
    expect(page.get_by_text("Plan activated!")).to_be_visible(timeout=10000)
    page.get_by_role("button", name="Close").click()
    expect(page.locator(".usage")).to_contain_text("Plus")
    expect(page.get_by_test_id("usage")).to_contain_text(re.compile(r"1[89],\d{3} / 20,000"))
    check("upgraded to Plus: 20,000/day limit in the UI", True)
    box.fill("I have a headache"); box.press("Enter")
    expect(page.get_by_text("[mock final]", exact=False).nth(1)).to_be_visible(timeout=15000)
    check("chat works again after upgrading", True)
    page.screenshot(path="/tmp/chat.png")

    # Hindi
    page.get_by_role("button", name="हिं").first.click()
    expect(page.get_by_test_id("usage")).to_contain_text("टोकन आज के लिए बचे")
    check("Hindi UI shows the daily wording", True)
    check("language cookie set for Clerk's screens", any(c["name"] == "sehat_saathi_lang" and c["value"] == "hi" for c in ctx.cookies()))

    # account deletion from the UI
    page.get_by_role("button", name="मेरा खाता हटाएँ").click()
    page.get_by_role("button", name="हाँ, सब हटा दें").click()
    page.wait_for_timeout(1500)
    deleted = open("/tmp/mock_clerk_deleted.txt").read().split()
    check("account deletion reached Clerk's Backend API", uid in deleted)

    check("no JavaScript errors", not errors)
    if errors: print(errors)
    browser.close()

print("\nBROWSER TEST", "PASSED" if ok else "FAILED")
sys.exit(0 if ok else 1)
