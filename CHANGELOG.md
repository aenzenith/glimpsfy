# Changelog

## [0.3.1](https://github.com/aenzenith/glimpsfy/compare/v0.3.0...v0.3.1) (2026-11-20)


### Bug Fixes

* **popover:** the panel eased between heights and its surfaces were refined ([dd5b3bb](https://github.com/aenzenith/glimpsfy/commit/dd5b3bbd97b35475471d60158d8569f4eb4c6f38))

## [0.3.0](https://github.com/aenzenith/glimpsfy/compare/v0.2.3...v0.3.0) (2026-11-20)


### Features

* **popover:** the picked capture got a large preview and labelled actions ([2574ef3](https://github.com/aenzenith/glimpsfy/commit/2574ef36389e024be98e737245a40908bc07c1c9))

## [0.2.3](https://github.com/aenzenith/glimpsfy/compare/v0.2.2...v0.2.3) (2026-11-20)


### Bug Fixes

* **capture:** captures failed while ScreenCaptureKit was busy ([#7](https://github.com/aenzenith/glimpsfy/issues/7)) ([da871f2](https://github.com/aenzenith/glimpsfy/commit/da871f2c1f88742b438ace075ef9d830ec4ebd4b))

## [0.2.2](https://github.com/aenzenith/glimpsfy/compare/v0.2.1...v0.2.2) (2026-11-18)


### Bug Fixes

* **capture:** a display whose page failed closed the capture later ([3f726fa](https://github.com/aenzenith/glimpsfy/commit/3f726fa259d7e5b988347492d15e4bcc76e2edb6))
* **firstrun:** replacing the macOS keys could leave Capture area keyless ([333ae74](https://github.com/aenzenith/glimpsfy/commit/333ae748265eedccd9fe36cf1329099820c1bb37))
* **history:** an upload ending after its entry was deleted lost its link ([e596300](https://github.com/aenzenith/glimpsfy/commit/e59630050069bc285bd51b72862a796c8d530a07))
* **history:** background text recognition kept an old image's text ([56e9c65](https://github.com/aenzenith/glimpsfy/commit/56e9c65f7153e372f31bb5492eb80625b77ecccc))
* **history:** export skipped long names and could replace a file ([05c1fdf](https://github.com/aenzenith/glimpsfy/commit/05c1fdf5b6629dc1ce8f938576fea239b490e471))
* **history:** search missed Chinese and Japanese words inside a sentence ([66c8a16](https://github.com/aenzenith/glimpsfy/commit/66c8a169137b9c568c2bd2f28756b986b1bd5176))
* **history:** upload and drag-out sent the older saved file of an entry ([edad402](https://github.com/aenzenith/glimpsfy/commit/edad402bd8f20707c978833af950f2f5d2799e09))
* **ocr:** the text layer could show text read before a redaction ([a1297fa](https://github.com/aenzenith/glimpsfy/commit/a1297fa497e072eaa4ddc25c9b83bce55caac629))
* **output:** a long title pushed {counter} out and later saves failed ([f4f6fe1](https://github.com/aenzenith/glimpsfy/commit/f4f6fe1a11934f9398eabf0da3f2122695854d20))
* **output:** saving all displays stopped at the first file that failed ([4d6cd66](https://github.com/aenzenith/glimpsfy/commit/4d6cd6666975d3b0f3baa40750ce721ee15030be))
* **pin:** saved captures did not pin and editor pins took a wrong scale ([867d46d](https://github.com/aenzenith/glimpsfy/commit/867d46d8bdaac3ae8b722e3b55dd1de66d22f10a))
* **record:** a quit during a take cut its file and kept it from history ([719623a](https://github.com/aenzenith/glimpsfy/commit/719623a72ea4c3c213136e82b478f3e46dc0d9ef))
* **record:** a source first heard late in a take landed twice as late ([aa3d065](https://github.com/aenzenith/glimpsfy/commit/aa3d065039c00f3724697a051510211f1b39cc73))
* **recorder:** the End key left the tail of a take out of the trim ([dd9952f](https://github.com/aenzenith/glimpsfy/commit/dd9952f2f3078488494f65d76832454dc166e800))
* **record:** Keep resumed a take the user had paused ([ce679cb](https://github.com/aenzenith/glimpsfy/commit/ce679cbe2cf18ab84a2e8f32960083c121311f5f))
* **settings:** an object where a plain value belongs reset all settings ([13811ea](https://github.com/aenzenith/glimpsfy/commit/13811ea2b5627c4d2a8907d0c3f75f302e842fbf))
* **shortcuts:** a key pressed as a countdown ended could freeze the app ([bc49b67](https://github.com/aenzenith/glimpsfy/commit/bc49b67824bb3ae5a5c4f84abce9075de5683945))
* **upload:** a .sxcu file with deeply nested calls crashed the app ([ea67fae](https://github.com/aenzenith/glimpsfy/commit/ea67fae6e66b8bd3cdda3f9c54057b93c2c0e514))
* **upload:** an untouched URL template blocked the custom uploader ([f9aecd2](https://github.com/aenzenith/glimpsfy/commit/f9aecd27f52a2d9dd3280fc51fc0c94cc12000cb))
* **upload:** consent to an https host covered the same host over http ([b7cb7fe](https://github.com/aenzenith/glimpsfy/commit/b7cb7fe7b7ba3985791633ef263473518117ef82))
* **upload:** empty .sxcu values went unsent and file names broke URLs ([5d69a4c](https://github.com/aenzenith/glimpsfy/commit/5d69a4ca38ed3470983c38114a03cc15447746fa))
* **upload:** keychain and consent failures showed the wrong error ([92dfaab](https://github.com/aenzenith/glimpsfy/commit/92dfaab0e125459b393eb5f43723ae07b3d16cb1))

## [0.2.1](https://github.com/aenzenith/glimpsfy/compare/v0.2.0...v0.2.1) (2026-11-18)


### Bug Fixes

* **capture:** a display plugged in during a capture showed the live desktop ([d64024a](https://github.com/aenzenith/glimpsfy/commit/d64024aa7ad4b2b655bb7f31135f5aee27c26cb4))
* **macos:** a Space switch froze the selection and slowed later captures ([3addc5f](https://github.com/aenzenith/glimpsfy/commit/3addc5f5717f41e342b66e5d2dd91601beb949ec))
* **output:** Save as could replace an existing file ([d0412c2](https://github.com/aenzenith/glimpsfy/commit/d0412c292448f19c229d869f297dafae4a1caa44))
* **record:** a cancel while the stream opened left it recording ([6578ef9](https://github.com/aenzenith/glimpsfy/commit/6578ef9f6abc729669beb4931c893a27019a6e17))


### Performance

* **overlay:** the selection moved without repainting the display ([bfec5e3](https://github.com/aenzenith/glimpsfy/commit/bfec5e39bc40563d674465791edb978dea19eff0))
* **webview:** pages loaded one language and woke less often ([478c42a](https://github.com/aenzenith/glimpsfy/commit/478c42ac03daaf7908851a3b875901fff19fe1cf))

## [0.2.0](https://github.com/aenzenith/glimpsfy/compare/v0.1.0...v0.2.0) (2026-11-17)


### Features

* **annotate:** hover frames, Cmd+A group selection and Cmd to draw over ([81d5bfd](https://github.com/aenzenith/glimpsfy/commit/81d5bfd290639bbcf10e362997f565f3d356da3e))
* **design:** chosen controls on glass became a raised lens ([a10f836](https://github.com/aenzenith/glimpsfy/commit/a10f83601ced3cecba755746d18e60c1593cc2a0))
* **editor:** tools moved to a side rail and options to a floating bar ([22b1dfa](https://github.com/aenzenith/glimpsfy/commit/22b1dfa0485b8c179d2003467757b3590729c940))
* **pin:** controls on a fading scrim that leaves with the pointer ([8db6ae6](https://github.com/aenzenith/glimpsfy/commit/8db6ae67879475b544c1fb21d659a4d003045b12))
* **toast:** compact toasts at the bottom right over native glass ([e375642](https://github.com/aenzenith/glimpsfy/commit/e3756422c508765843dbe4cb91336a5579ed843d))


### Bug Fixes

* **macos:** a capture opened on another Space after the displays changed ([f824012](https://github.com/aenzenith/glimpsfy/commit/f824012f2d4422891cd425d215ef5153841d5774))
* **overlay:** copy and save failed after the pointer changed display ([e3be62d](https://github.com/aenzenith/glimpsfy/commit/e3be62d15eca25879b146f6d3cef273319f79f16))
* **popover:** picked thumbnails no longer lifted under the pointer ([561fcf2](https://github.com/aenzenith/glimpsfy/commit/561fcf2d104965c492d370d781ba8947e11048bf))
* **recorder:** export buttons overflowed the review panel ([573387f](https://github.com/aenzenith/glimpsfy/commit/573387f3f764f2b111540966442c65e7f32bbfd9))
* **record:** setup bar lacked its glass and jumped as menus opened ([6425ff7](https://github.com/aenzenith/glimpsfy/commit/6425ff7a5fdef08bec400b7120042176a6fc3ebe))
* **webview:** a capture dropped on the popover replaced its page ([8c6fc85](https://github.com/aenzenith/glimpsfy/commit/8c6fc85c26a1537353c43747a7488e333921c4ba))
* **windows:** menu bar windows opened on the main display ([94fb67b](https://github.com/aenzenith/glimpsfy/commit/94fb67b09cc6f2f60f2e88f6f3489e7135ef23e0))

## 0.1.0 (2026-11-17)


### Features

* Glimpsfy, the first release ([781e6ee](https://github.com/aenzenith/glimpsfy/commit/781e6ee00d5169c3a5594c3fad95c035c6a1b916))
