"""Opt-in multi-account v3 development UI acceptance; never run before deployment.

Requires a disposable existing session with sharing enabled and two active test
accounts. Credentials come only from environment, never arguments or output:
V3_CHAT_OWNER_USER/PASSWORD and V3_CHAT_JOIN_USER/PASSWORD. Optional observer
V3_CHAT_OBSERVER_USER/PASSWORD verifies read-only access when configured.
Run on the development server with Selenium/Chromium in its approved environment.
No arbitrary browser JavaScript, token exports, provider calls, or production URLs.
"""
import argparse
import os
from pathlib import Path


def run(session_id, output):
    from selenium import webdriver
    from selenium.webdriver.common.by import By
    from selenium.webdriver.support.ui import WebDriverWait
    from selenium.webdriver.support import expected_conditions as EC
    origin = 'https://dev.webagent.live'
    drivers = []

    def browser():
        options = webdriver.ChromeOptions()
        options.add_argument('--headless=new')
        options.add_argument('--disable-dev-shm-usage')
        options.add_argument('--window-size=1280,900')
        driver = webdriver.Chrome(options=options)
        drivers.append(driver)
        return driver

    def wait(driver):
        return WebDriverWait(driver, 25)

    def visible(driver, selector):
        return wait(driver).until(EC.visibility_of_element_located((By.CSS_SELECTOR, selector)))

    def login(driver, prefix, route):
        driver.get(origin + route)
        visible(driver, '[data-private-access-reveal]')
        reveal = driver.find_elements(By.CSS_SELECTOR, '[data-private-access-reveal]')
        if reveal and reveal[0].is_displayed():
            reveal[0].click()
        user = visible(driver, '#login-username')
        user.send_keys(os.environ[prefix + '_USER'])
        driver.find_element(By.CSS_SELECTOR, '#login-password').send_keys(os.environ[prefix + '_PASSWORD'])
        driver.find_element(By.CSS_SELECTOR, '#login-submit').click()
        wait(driver).until(lambda d: not d.find_element(By.CSS_SELECTOR, '#app').get_attribute('hidden'))

    def control(driver, name):
        return visible(driver, '[data-header-control="' + name + '"]')

    try:
        owner = browser()
        login(owner, 'V3_CHAT_OWNER', '/app?session_id=' + session_id)
        active = control(owner, 'agent_active')
        wait(owner).until(lambda d: active.is_enabled())
        if active.get_attribute('aria-pressed') == 'true':
            active.click()
        wait(owner).until(lambda d: control(d, 'agent_active').get_attribute('aria-pressed') == 'false')
        control(owner, 'share').click()
        dialog = visible(owner, '.v3-shared-chat-panel')
        wait(owner).until(EC.element_to_be_clickable((By.XPATH, "//section[contains(@class, 'v3-shared-chat-panel')]/button[normalize-space(.)='Create invitation']"))).click()
        link = visible(owner, 'input[aria-label="Single-use invitation link"]').get_attribute('value')
        panel_rect = owner.find_element(By.CSS_SELECTOR, '#chat-panel').rect
        assert dialog.rect['y'] + dialog.rect['height'] <= panel_rect['y'] + panel_rect['height'] + 1
        wait(owner).until(EC.presence_of_element_located((By.XPATH, "//section[contains(@class, 'v3-shared-chat-panel')]/button[normalize-space(.)='Close']"))).click()
        wait(owner).until(EC.invisibility_of_element_located((By.CSS_SELECTOR, '.v3-shared-chat-panel')))
        assert control(owner, 'share').get_attribute('aria-expanded') == 'false'
        assert link.startswith(origin + '/app?chat_invite=')

        joined = browser()
        login(joined, 'V3_CHAT_JOIN', link[len(origin):])
        wait(joined).until(lambda d: 'chat_invite=' not in d.current_url and 'session_id=' + session_id in d.current_url)
        wait(joined).until(lambda d: control(d, 'agent_active').get_attribute('aria-pressed') == 'false')
        assert visible(joined, '#chat-input').is_enabled()
        sentinel = 'Shared chat browser acceptance: human conversation while inactive.'
        visible(joined, '#chat-input').send_keys(sentinel)
        visible(joined, '#chat-send').click()
        wait(owner).until(lambda d: sentinel in visible(d, '#chat-messages-inner').text)
        wait(joined).until(lambda d: sentinel in visible(d, '#chat-messages-inner').text)
        assert control(owner, 'agent_active').get_attribute('aria-pressed') == 'false'
        bubble = next(row for row in owner.find_elements(By.CSS_SELECTOR, '.chat-bubble.user') if sentinel in row.text)
        assert bubble.find_element(By.CSS_SELECTOR, '.label').text != 'You'
        wait(owner).until(lambda d: 'joined this chat' in visible(d, '#chat-messages-inner').text)
        control(joined, 'agent_active').click()
        wait(owner).until(lambda d: control(d, 'agent_active').get_attribute('aria-pressed') == 'true')
        owner.refresh()
        wait(owner).until(lambda d: control(d, 'agent_active').get_attribute('aria-pressed') == 'true')
        assert 'session_id=' + session_id in owner.current_url
        wait(owner).until(lambda d: sentinel in visible(d, '#chat-messages-inner').text)

        if os.environ.get('V3_CHAT_OBSERVER_USER'):
            observer = browser()
            login(observer, 'V3_CHAT_OBSERVER', '/app?session_id=' + session_id)
            wait(observer).until(lambda d: visible(d, '#chat-input').get_attribute('readonly') is not None)
            assert not control(observer, 'agent_active').is_enabled()
            assert not control(observer, 'share').is_enabled()
            send = observer.find_element(By.CSS_SELECTOR, '#chat-send')
            assert not send.is_displayed() or not send.is_enabled()

        output.mkdir(parents=True, exist_ok=True)
        owner.save_screenshot(str(output / 'shared-chat-desktop.png'))
        joined.set_window_size(390, 844)
        joined.refresh()
        wait(joined).until(lambda d: control(d, 'agent_active').is_displayed())
        joined.save_screenshot(str(output / 'shared-chat-mobile.png'))
        print('PASS: invitation, full shared transcript, attribution-visible message, shared mode persistence, and mobile controls')
    finally:
        for driver in drivers:
            driver.quit()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--session-id', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    run(args.session_id, args.output)
