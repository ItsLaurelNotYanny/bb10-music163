import bb.cascades 1.2

Page {
    id: rootPage

    Menu.definition: MenuDefinition {
        actions: [
            ActionItem {
                title: "刷新"
                imageSource: "asset:///icons/refresh.png"
                onTriggered: {
                    var target = shellConfig.webViewUrl
                    playerWebView.url = ""
                    playerWebView.url = target
                }
            },
            ActionItem {
                title: "外网 OCI"
                imageSource: "asset:///icons/cloud.png"
                onTriggered: {
                    shellConfig.setGatewayUrl(shellConfig.defaultRemoteUrl())
                }
            },
            ActionItem {
                title: "内网 NAS"
                imageSource: "asset:///icons/home.png"
                onTriggered: {
                    shellConfig.setGatewayUrl(shellConfig.defaultLanUrl())
                }
            }
        ]
        settingsAction: SettingsActionItem {
            title: "网关地址"
            imageSource: "asset:///icons/settings.png"
            onTriggered: {
                urlInput.text = shellConfig.gatewayUrl
                urlSheet.open()
            }
        }
    }

    Container {
        layout: DockLayout {}
        background: Color.create("#141416")

        WebView {
            id: playerWebView
            horizontalAlignment: HorizontalAlignment.Fill
            verticalAlignment: VerticalAlignment.Fill
            url: shellConfig.webViewUrl
        }
    }

    attachedObjects: [
        Sheet {
            id: urlSheet
            Page {
                titleBar: TitleBar {
                    title: "Gateway 地址设置"
                    dismissAction: ActionItem {
                        title: "取消"
                        onTriggered: {
                            urlSheet.close()
                        }
                    }
                    acceptAction: ActionItem {
                        title: "保存"
                        onTriggered: {
                            shellConfig.setGatewayUrl(urlInput.text)
                            urlSheet.close()
                        }
                    }
                }
                Container {
                    topPadding: 24
                    leftPadding: 24
                    rightPadding: 24
                    Label {
                        text: "输入播放器 URL（内网 HTTP 16381 或外网 OCI HTTPS 16380）："
                        multiline: true
                    }
                    TextField {
                        id: urlInput
                        hintText: "https://gateway.example.com:16380/player"
                        inputMode: TextFieldInputMode.Url
                    }
                }
            }
        }
    ]
}
