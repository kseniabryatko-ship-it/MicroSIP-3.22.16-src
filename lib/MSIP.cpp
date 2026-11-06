/*
 * Copyright (C) 2011-2025 MicroSIP (http://www.microsip.org)
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 2 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software
 * Foundation, Inc., 59 Temple Place, Suite 330, Boston, MA  02111-1307  USA
 */

#include "stdafx.h"

#define THIS_FILENAME "MSIP.cpp"

#include "MSIP.h"
using namespace MSIP;

#include "langpack.h"
#include "settings.h"

#include <Netlistmgr.h>
#include <sddl.h>

CStringA msip_md5sum(const CStringA& str)
{
    CStringA md5sum;
    DWORD cbContent = str.GetLength();
    BYTE* pbContent = (BYTE*)str.GetString();
    pj_md5_context ctx;
    pj_uint8_t digest[16];
    pj_md5_init(&ctx);
    pj_md5_update(&ctx, (pj_uint8_t*)pbContent, cbContent);
    pj_md5_final(&ctx, digest);
    char* p = md5sum.GetBuffer(32);
    for (int i = 0; i < 16; ++i) {
        pj_val_to_hex_digit(digest[i], p);
        p += 2;
    }
    md5sum.ReleaseBuffer();
    return md5sum;
}

CStringA msip_md5sum(const CString& str)
{
    return msip_md5sum(Utf8EncodeUni(str));
}

//void msip_audio_output_set_volume(int val, bool mute)
//{
//	if (mute) {
//		val = 0;
//	} else {
//		pj_status_t status = 
//			pjsua_snd_set_setting(
//			PJMEDIA_AUD_DEV_CAP_OUTPUT_VOLUME_SETTING,
//			&val, PJ_TRUE);
//		if (status == PJ_SUCCESS) {
//			val = 100;
//		}
//	}
//	pjsua_conf_adjust_tx_level(0, (float)val/100);
//}

void msip_audio_conf_set_volume(int val, bool mute)
{
    if (!is_pjsua_running()) {
        return;
    }
    if (mute) {
        val = 0;
    }
    pjsua_call_id call_ids[PJSUA_MAX_CALLS];
    unsigned count = PJSUA_MAX_CALLS;
    if (pjsua_enum_calls(call_ids, &count) == PJ_SUCCESS) {
        for (unsigned i = 0; i < count; ++i) {
            pjsua_conf_port_id conf_port_id = pjsua_call_get_conf_port(call_ids[i]);
            if (conf_port_id != PJSUA_INVALID_ID) {
                pjsua_conf_adjust_rx_level(conf_port_id, (float)val / 100);
            }
        }
    }
}

void msip_audio_input_set_volume(int val, bool mute)
{
    if (!is_pjsua_running()) {
        return;
    }
    if (mute) {
        val = 0;
    }
    else {
        pj_status_t status = -1;
        if (!accountSettings.swLevelAdjustment) {
            int valHW;
            if (accountSettings.micAmplification) {
                valHW = val >= 50 ? 100 : val * 2;
            }
            else {
                valHW = val;
            }
            status =
                pjsua_snd_set_setting(
                    PJMEDIA_AUD_DEV_CAP_INPUT_VOLUME_SETTING,
                    &valHW, PJ_TRUE);
        }
        if (status == PJ_SUCCESS) {
            if (accountSettings.micAmplification && val > 50) {
                val = 100 + pow((float)val - 50, 1.68f);
            }
            else {
                val = 100;
            }
        }
        else {
            if (accountSettings.micAmplification) {
                if (val > 50) {
                    val = 50 + pow((float)val - 50, 1.7f);
                }
            }
        }
    }
    pjsua_conf_adjust_rx_level(0, (float)val / 100);
}

pj_status_t msip_verify_sip_url(const CString& url)
{
    if (!is_pjsua_running()) {
        return PJSIP_ENOTINITIALIZED;
    }
    CStringA urlA = Utf8EncodeUni(url);
    return urlA.GetLength() > 900 ? PJSIP_EURITOOLONG : pjsua_verify_sip_url(urlA);
}

int msip_get_duration(pj_time_val* time_val)
{
    int res = time_val->sec;
    if (time_val->msec >= 500) {
        res++;
    }
    return res;
}

void MSIP::GetScreenRect(CRect* rect)
{
    rect->left = GetSystemMetrics(SM_XVIRTUALSCREEN);
    rect->top = GetSystemMetrics(SM_YVIRTUALSCREEN);
    rect->right = GetSystemMetrics(SM_CXVIRTUALSCREEN) - rect->left;
    rect->bottom = GetSystemMetrics(SM_CYVIRTUALSCREEN) - rect->top;
}

CString MSIP::GetErrorMessage(pj_status_t status, CString &descr)
{
    CStringA str;
    if (!is_pjsua_running()) {
        str = "Softphone is not initialized. Check your settings.";
    }
    else if (status == 171039 || status == 171042) {
        str = "Invalid Number";
    }
    else if (status == PJSIP_EAUTHACCNOTFOUND || status == PJSIP_EAUTHACCDISABLED) {
        str = "Account or credentials not found.";
        descr = Translate(CString("Activate or add an account in the main menu of the softphone."));
        descr.AppendFormat(_T("\n\n%s"), Translate(_T("If you don't have your account details, contact your IP telephony provider to obtain them.")));
    }
    else if (status == 130051) {
        str = "Unable to connect to the remote server.";
    }
    else if (status == 130022) {
        str = "Unable to send the network request.";
    }
    else {
        char* buf = str.GetBuffer(PJ_ERR_MSG_SIZE - 1);
        pj_strerror(status, buf, PJ_ERR_MSG_SIZE);
        str.ReleaseBuffer();
        int i = str.ReverseFind('(');
        if (i != -1) {
            str = str.Left(i - 1);
        }
    }
    return Translate(CString(str));
}

BOOL MSIP::ShowErrorMessage(CWnd* wnd, pj_status_t status)
{
    if (status != PJ_SUCCESS) {
        CString descr;
        CString message = GetErrorMessage(status, descr);
        TaskDialog(wnd->GetSafeHwnd(), AfxGetInstanceHandle(),
            _T(_GLOBAL_NAME_NICE),
            message,
            descr,
            TDCBF_OK_BUTTON,
            TD_WARNING_ICON, NULL);
        return TRUE;
    }
    else {
        return FALSE;
    }
}

CString MSIP::RemovePort(const CString& domain)
{
    int pos = domain.Find(_T(":"));
    if (pos != -1) {
        return domain.Mid(0, pos);
    }
    else {
        return domain;
    }
}

bool MSIP::IsIP(const CString& host)
{
    if (host.IsEmpty())
        return false;

    CStringA hostA(host);

    IN_ADDR v4 = {};
    IN6_ADDR v6 = {};

    // IPv4 check
    if (InetPtonA(AF_INET, hostA, &v4) == 1)
        return true;

    // IPv6 check
    if (InetPtonA(AF_INET6, hostA, &v6) == 1)
        return true;

    return false;
}

bool MSIP::ParseSIPURI(const CString& text, SIPURI& uri)
{
    uri = SIPURI();

    CString s = text;
    s.Trim();

    //---------------------------------------------------
    // Display Name
    //---------------------------------------------------

    int lt = s.Find(L'<');
    int gt = s.ReverseFind(L'>');

    CString uriText;

    if (lt >= 0 && gt > lt)
    {
        uri.name = s.Left(lt);
        uri.name.Trim();

        if (uri.name.GetLength() >= 2 &&
            uri.name[0] == '"' &&
            uri.name[uri.name.GetLength() - 1] == '"')
        {
            uri.name = uri.name.Mid(1, uri.name.GetLength() - 2);
        }
        if (uri.name.CompareNoCase(_T("unknown"))==0)
        {
            uri.name.Empty();
        }
        uriText = s.Mid(lt + 1, gt - lt - 1);
    }
    else
    {
        uriText = s;
    }

    //---------------------------------------------------
    // scheme
    //---------------------------------------------------

    if (uriText.Left(4).CompareNoCase(L"sip:") == 0)
        uriText = uriText.Mid(4);

    if (uriText.Left(5).CompareNoCase(L"sips:") == 0)
        uriText = uriText.Mid(5);

    //---------------------------------------------------
    // suffix
    //---------------------------------------------------

    int semi = uriText.Find(L';');

    if (semi >= 0)
    {
        uri.suffix = uriText.Mid(semi);
        uriText = uriText.Left(semi);
    }
    else {
        int q = uriText.Find(L'?');

        if (q >= 0)
        {
            uri.suffix = uriText.Mid(q);
            uriText = uriText.Left(q);
        }
    }

    //---------------------------------------------------
    // user / domain
    //---------------------------------------------------

    int at = uriText.Find(L'@');

    if (at >= 0)
    {
        uri.user = uriText.Left(at);
        uri.domain = uriText.Mid(at + 1);
    }
    else
    {
        uri.domain = uriText;
    }

    return !uri.domain.IsEmpty();
}

CString MSIP::BuildSIPURI(const SIPURI &uri)
{
    CString res;
    res.Format(_T("%s@%s"), uri.user, uri.domain);
    if (!uri.suffix.IsEmpty()) {
        res.Append(uri.suffix);
    }
    CString tmp = res;
    res.Format(_T("<sip:%s>"), tmp);
    if (!uri.name.IsEmpty()) {
        tmp = res;
        res.Format(_T("\"%s\" %s"), uri.name, tmp);
    }
    return res;
}

CString MSIP::AddrToNumber(const CString &addr)
{
    CString res;
    SIPURI sipuri;
    MSIP::ParseSIPURI(addr, sipuri);
    if (sipuri.user.IsEmpty()) {
        res = sipuri.domain;
    }
    else {
        res = sipuri.user;
    }
    return res;
}

CString MSIP::PjToStr(const pj_str_t* str, BOOL utf)
{
    CStringA rab;
    rab.Format("%.*s", str->slen, str->ptr);
    if (utf) {
        return MSIP::Utf8DecodeUni(rab);
    }
    else {
        return CString(rab);
    }
}

CString MSIP::Utf8DecodeUni(const char* str)
{
    CString res;

    if (!str)
        return res;

    int len = MultiByteToWideChar(
        CP_UTF8,
        0,
        str,
        -1,
        nullptr,
        0);

    if (len <= 0)
        return res;

    wchar_t* buf = res.GetBuffer(len);

    MultiByteToWideChar(
        CP_UTF8,
        0,
        str,
        -1,
        buf,
        len);

    res.ReleaseBuffer();
    return res;
}

CStringA MSIP::Utf8EncodeUni(const CString& str)
{
    CStringA res;
    char* msg = WideCharToPjStr(str);
    res = msg;
    free(msg);
    return res;
}

CStringA MSIP::UnicodeToAnsi(const CString& str)
{
    CStringA res;
    int nCount = str.GetLength();
    for (int nIdx = 0; nIdx < nCount; nIdx++)
    {
        res += str[nIdx];
    }
    return res;
}

CString MSIP::AnsiToUnicode(const CStringA& str)
{
    CString res;
    int nCount = str.GetLength();
    for (int nIdx = 0; nIdx < nCount; nIdx++)
    {
        res += str[nIdx];
    }
    return res;
}

CString MSIP::AnsiToWideChar(const char* str)
{
    CString res;
    int iNeeded = MultiByteToWideChar(CP_ACP, 0, str, -1, 0, 0);
    wchar_t* wlocal = res.GetBuffer((iNeeded + 1) * sizeof(wchar_t));
    MultiByteToWideChar(CP_ACP, 0, str, -1, wlocal, iNeeded);
    res.ReleaseBuffer();
    return res;
}

CStringA MSIP::StringToPjString(const CString& str)
{
    CStringA res;
    int len = str.GetLength() * 4;
    char* buf = res.GetBuffer(len);
    pj_unicode_to_ansi(str.GetString(), -1, buf, len + 1);
    res.ReleaseBuffer();
    return res;
}

pj_str_t MSIP::StrToPjStr(const CString& str)
{
    // do not forget to free memory after call
    return pj_str(WideCharToPjStr(str));
}

char* MSIP::WideCharToPjStr(const CString& str)
{
    // do not forget to free memory after call
    int len = str.GetLength() * 4;
    char* buf = (char*)malloc(len + 1);
    pj_unicode_to_ansi(str.GetString(), -1, buf, len + 1);
    return buf;
}

void MSIP::OpenURL(const CString& url)
{
    CString param;
    param.Format(_T("url.dll,FileProtocolHandler %s"), url);
    ShellExecute(NULL, NULL, _T("rundll32.exe"), param, NULL, SW_SHOWNORMAL);
}

void MSIP::OpenFile(const CString& filename)
{
    ShellExecute(NULL, NULL, filename, NULL, NULL, SW_SHOWNORMAL);
}

CString MSIP::GetDuration(int sec, bool zero)
{
    CString duration;
    if (sec || zero) {
        int h, m, s;
        s = sec;
        h = s / 3600;
        s = s % 3600;
        m = s / 60;
        s = s % 60;
        if (h) {
            duration.Format(_T("%d:%02d:%02d"), h, m, s);
        }
        else {
            duration.Format(_T("%d:%02d"), m, s);
        }
    }
    return duration;
}

bool MSIP::IsPSTNNnmber(const CString& number)
{
    bool isDigits = true;
    int len = number.GetLength();
    if (len < 4) return false;
    for (int i = 0; i < len; i++)
    {
        if ((number[i] > '9' || number[i] < '0') && number[i] != '*' && number[i] != '#' && number[i] != '.' && number[i] != '-' && number[i] != '(' && number[i] != ')' && number[i] != '/' && number[i] != ' ' && number[0] != '+')
        {
            isDigits = false;
            break;
        }
    }
    return isDigits;
}

int MSIP::GetNormalizedNumberOffset(const CString& number)
{
    int offset;
    int len = number.GetLength();
    if (len >= 8 && wcsncmp(number.GetString(), _T("+"), 1) == 0) {
        offset = 1;
    }
    else if (len >= 9 && wcsncmp(number.GetString(), _T("00"), 2) == 0) {
        offset = 2;
    }
    else if (len >= 10 && (wcsncmp(number.GetString(), _T("011"), 3) == 0 || wcsncmp(number.GetString(), _T("810"), 3) == 0)) {
        offset = 3;
    }
    else if (len >= 8 && (wcsncmp(number.GetString(), _T("0"), 1) == 0 || wcsncmp(number.GetString(), _T("8"), 1) == 0)) {
        offset = 1;
    }
    else {
        offset = 0;
    }
    return offset;
}

bool MSIP::IniSectionExists(const CString& section, const CString& iniFile)
{
    CString str;
    LPTSTR ptr = str.GetBuffer(3);
    int result = GetPrivateProfileString(section, NULL, NULL, ptr, 3, iniFile);
    str.ReleaseBuffer();
    return result;
}

CString MSIP::Bin2String(CByteArray* ca)
{
    CString res;
    int k = ca->GetSize();
    for (int i = 0; i < k; i++) {
        unsigned char ch = ca->GetAt(i);
        res.AppendFormat(_T("%02x"), ca->GetAt(i));
    }
    return res;
}

void MSIP::String2Bin(const CString& str, CByteArray* res)
{
    res->RemoveAll();
    int k = str.GetLength();
    CStringA rab;
    for (int i = 0; i < str.GetLength(); i += 2) {
        rab = CStringA(str.Mid(i, 2));
        char* p = NULL;
        unsigned long bin = strtoul(rab.GetString(), &p, 16);
        res->Add(bin);
    }
}

void MSIP::CommandLineToShell(CString cmd, CString& command, CString& params)
{
    cmd.Trim();
    command.Empty();
    params.Empty();
    int nArgs;
    LPWSTR* szArglist = CommandLineToArgvW(cmd, &nArgs);
    if (NULL == szArglist) {
        AfxMessageBox(_T("Wrong command: ") + cmd);
    }
    else for (int i = 0; i < nArgs; i++) {
        if (!i) {
            command = szArglist[i];
        }
        else {
            params.AppendFormat(_T("%s "), szArglist[i]);
        }
    }
    params.TrimRight();
    LocalFree(szArglist);
}

void MSIP::RunCmd(const CString& cmdLine, const CString& addParams, bool noWait)
{
    CString str, command, params;
    //if (cmdLine.Find('"') == -1) {
    //CString tmp = cmdLine;
    //cmdLine.Format(_T("\"%s\""), tmp);
    //}
    CommandLineToShell(cmdLine, command, params);
    params.AppendFormat(_T(" %s"), addParams);
    params.TrimLeft();

    SHELLEXECUTEINFO ShExecInfo = { 0 };
    ShExecInfo.cbSize = sizeof(SHELLEXECUTEINFO);
    ShExecInfo.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
    ShExecInfo.hwnd = NULL;
    ShExecInfo.lpVerb = NULL;
    ShExecInfo.lpFile = command;
    ShExecInfo.lpParameters = params;
    ShExecInfo.lpDirectory = NULL;
    ShExecInfo.nShow = SW_HIDE;
    ShExecInfo.hInstApp = NULL;
    ShellExecuteEx(&ShExecInfo);
    if (!noWait) {
        DWORD res = WaitForSingleObject(ShExecInfo.hProcess, 10000);
    }
    CloseHandle(ShExecInfo.hProcess);
}

bool MSIP::IsNat64(const sockaddr_in6* addr)
{
    const BYTE prefix[] =
    {
        0x00, 0x64, 0xff, 0x9b,
        0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00
    };

    return memcmp(addr->sin6_addr.s6_addr, prefix, 12) == 0;
}

void MSIP::PortKnock()
{
    if (accountSettings.portKnockerPorts.IsEmpty())
        return;

    CString host;

    if (!accountSettings.portKnockerHost.IsEmpty())
        host = accountSettings.portKnockerHost;
    else
        host = MSIP::RemovePort(get_account_server());

    if (host.IsEmpty())
        return;

    CStringA hostA(host);

    addrinfo hints = {};
    hints.ai_family = AF_UNSPEC;       // IPv4 + IPv6
    hints.ai_socktype = SOCK_DGRAM;
    hints.ai_protocol = IPPROTO_UDP;

    addrinfo* result = nullptr;

    if (getaddrinfo(hostA, nullptr, &hints, &result) != 0 || !result)
    {
        return;
    }

    bool bHasIp4 = false;
    for (addrinfo* ai = result; ai; ai = ai->ai_next)
    {
        if (ai->ai_family == AF_INET) {
            bHasIp4 = true;
            break;
        }
    }

    const char buf[] = "knock";

    int pos = 0;
    CString strPort =
        accountSettings.portKnockerPorts.Tokenize(_T(","), pos);

    while (pos != -1)
    {
        strPort.Trim();

        if (!strPort.IsEmpty())
        {
            int port = StrToInt(strPort);

            if (port > 0 && port <= 65535)
            {
                for (addrinfo* ai = result; ai; ai = ai->ai_next)
                {
                    SOCKET s = socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);

                    if (s == INVALID_SOCKET)
                        continue;

                    if (ai->ai_family == AF_INET)
                    {
                        sockaddr_in addr = *(sockaddr_in*)ai->ai_addr;
                        addr.sin_port = htons((u_short)port);

                        int res = sendto(
                            s,
                            buf,
                            (int)sizeof(buf),
                            0,
                            (sockaddr*)&addr,
                            sizeof(addr));
                        res++;
                    }
                    else if (ai->ai_family == AF_INET6)
                    {
                        sockaddr_in6 addr = *(sockaddr_in6*)ai->ai_addr;
                        
                        if (bHasIp4 && MSIP::IsNat64(&addr))
                            continue;

                        addr.sin6_port = htons((u_short)port);

                        int res = sendto(
                            s,
                            buf,
                            (int)sizeof(buf),
                            0,
                            (sockaddr*)&addr,
                            sizeof(addr));
                        res++;
                    }

                    closesocket(s);
                }

                Sleep(200);
            }
        }

        strPort =
            accountSettings.portKnockerPorts.Tokenize(_T(","), pos);
    }

    freeaddrinfo(result);
}

bool MSIP::IsConnectedToNetworkCOM(bool bCheckInternet)
{
    INetworkListManager* pNetworkListManager = nullptr;

    HRESULT hr = CoCreateInstance(
        CLSID_NetworkListManager,
        NULL,
        CLSCTX_ALL,
        __uuidof(INetworkListManager),
        (LPVOID*)&pNetworkListManager);

    if (FAILED(hr) || !pNetworkListManager)
        return false;

    VARIANT_BOOL isConnected = VARIANT_FALSE;

    if (bCheckInternet)
        hr = pNetworkListManager->get_IsConnectedToInternet(&isConnected);
    else
        hr = pNetworkListManager->get_IsConnected(&isConnected);

    pNetworkListManager->Release();

    return SUCCEEDED(hr) && isConnected == VARIANT_TRUE;
}

MIB_IPFORWARD_ROW2 oldRoute = {};
SOCKADDR_INET oldSource = {};
bool MSIP::IsNetworkChanged()
{
    CString host = MSIP::RemovePort(get_account_server());
    if (host.IsEmpty() || !MSIP::IsIP(host)) {
        host = _T("8.8.8.8");
    }
    SOCKADDR_INET dst = {};
    dst.si_family = AF_INET;
    InetPton(AF_INET, host, &dst.Ipv4.sin_addr);
    MIB_IPFORWARD_ROW2 route = {};
    SOCKADDR_INET source = {};
    DWORD err = GetBestRoute2(
        NULL,
        0,
        NULL,
        &dst,
        0,
        &route,
        &source);
    if (err != NO_ERROR) {
        return false;
    }
    bool changed =
        oldRoute.InterfaceLuid.Value != route.InterfaceLuid.Value ||
        memcmp(&oldRoute.NextHop, &route.NextHop, sizeof(SOCKADDR_INET)) != 0 ||
        memcmp(&oldSource, &source, sizeof(SOCKADDR_INET)) != 0;
    if (changed) {
        oldRoute.InterfaceLuid = route.InterfaceLuid;
        memcpy(&oldRoute.NextHop, &route.NextHop, sizeof(SOCKADDR_INET));
        memcpy(&oldSource, &source, sizeof(SOCKADDR_INET));
    }
    return changed;
}

CString MSIP::FormatDateTime(CTime* pTime, CTime* pTimeNow)
{
    CTime timeNow;
    if (!pTimeNow) {
        timeNow = CTime::GetCurrentTime();
        pTimeNow = &timeNow;
    }
    return pTime->Format(
        pTimeNow->GetYear() == pTime->GetYear() &&
        pTimeNow->GetMonth() == pTime->GetMonth() &&
        pTimeNow->GetDay() == pTime->GetDay()
        ? _T("%X") : _T("%c")
    );
}

void MSIP::ExpandEnvironmentStrings(CString& str)
{
    if (!str.IsEmpty()) {
        DWORD len = ExpandEnvironmentStrings(str, 0, 0);
        CString out;
        LPWSTR ptr = out.GetBuffer(len);
        ExpandEnvironmentStrings(str, ptr, len);
        out.ReleaseBuffer();
        str = out;
    }
}

CString MSIP::GetSID() {
    CString res;
    HANDLE hToken;
    if (OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &hToken)) {
        DWORD size = 0;
        GetTokenInformation(hToken, TokenUser, nullptr, 0, &size);
        TOKEN_USER* tokenUser = (TOKEN_USER*) new BYTE[size];
        if (GetTokenInformation(hToken, TokenUser, tokenUser, size, &size)) {
            LPWSTR stringSid = nullptr;
            ConvertSidToStringSid(tokenUser->User.Sid, &stringSid);
            res = stringSid;
            LocalFree(stringSid);
        }
        CloseHandle(hToken);
        delete[](BYTE*)tokenUser;
    }
    return res;
}

CString MSIP::MakeFilenameFromString(const CString& str)
{
    CString res = str;
    res.Trim();
    res.MakeLower();
    while (res.Replace(_T("  "), _T(" ")));
    res.Replace(' ', '-');
    return res;
}

int MSIP::CheckRegistryValue(HKEY root, const CString& path)
{
    HKEY hKey;
    LONG status;

    status = RegOpenKeyExW(root, path, 0, KEY_READ | KEY_WOW64_64KEY, &hKey);
    if (status != ERROR_SUCCESS)
        return 1;

    wchar_t value[32];
    DWORD size = sizeof(value);
    if (RegQueryValueExW(hKey, L"Value", NULL, NULL, (LPBYTE)value, &size) != ERROR_SUCCESS)
    {
        RegCloseKey(hKey);
        return 1;
    }
    RegCloseKey(hKey);

    return (_wcsicmp(value, L"Deny") != 0);
}

int MSIP::IsCameraAccessAllowed()
{
    CString path = L"Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\webcam";
    return CheckRegistryValue(HKEY_LOCAL_MACHINE, path)
        && CheckRegistryValue(HKEY_CURRENT_USER, path)
        && CheckRegistryValue(HKEY_CURRENT_USER, path + L"\\NonPackaged");
}

