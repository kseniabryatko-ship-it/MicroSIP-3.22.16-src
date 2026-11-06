/*
 * Copyright (C) 2011-2026 MicroSIP (http://www.microsip.org)
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
#include "MessagesDlg.h"
#include "microsip.h"
#include "mainDlg.h"
#include "settings.h"
#include "Transfer.h"
#include "langpack.h"

static DWORD CALLBACK MEditStreamOutCallback(DWORD_PTR dwCookie, LPBYTE pbBuff, LONG cb, LONG* pcb)
{
    CString sThisWrite;
    sThisWrite.GetBufferSetLength(cb);

    CString* psBuffer = (CString*)dwCookie;

    for (int i = 0; i < cb; i++)
    {
        sThisWrite.SetAt(i, *(pbBuff + i));
    }

    *psBuffer += sThisWrite;

    *pcb = sThisWrite.GetLength();
    sThisWrite.ReleaseBuffer();
    return 0;
}

static DWORD CALLBACK MEditStreamInCallback(DWORD_PTR dwCookie, LPBYTE pbBuff, LONG cb, LONG* pcb)
{
    CString* psBuffer = (CString*)dwCookie;

    if (cb > psBuffer->GetLength()) cb = psBuffer->GetLength();

    for (int i = 0; i < cb; i++)
    {
        *(pbBuff + i) = psBuffer->GetAt(i);
    }

    *pcb = cb;
    *psBuffer = psBuffer->Mid(cb);

    return 0;
}

MessagesDlg::MessagesDlg(CWnd* pParent /*=NULL*/)
    : CBaseDialog(MessagesDlg::IDD, pParent)
{
    this->m_hWnd = NULL;
    Create(IDD, pParent);
}

MessagesDlg::~MessagesDlg(void)
{
}

int MessagesDlg::OnCreate(LPCREATESTRUCT lpCreateStruct)
{
    if (langPack.rtl) {
        ModifyStyleEx(0, WS_EX_LAYOUTRTL);
    }
    return 0;
}

void MessagesDlg::DoDataExchange(CDataExchange* pDX)
{
    CBaseDialog::DoDataExchange(pDX);
    DDX_Control(pDX, IDC_MESSAGES_TAB, m_messagesTab);
    DDX_Control(pDX, IDC_CALL_END, m_ButtonCall);
    DDX_Control(pDX, IDC_END, m_ButtonEnd);
}

BOOL MessagesDlg::OnInitDialog()
{
    CBaseDialog::OnInitDialog();

    AutoMove(IDC_MESSAGES_TAB, 0, 0, 100, 0);
    AutoMove(IDC_LAST_CALL, 100, 0, 0, 0);
    AutoMove(IDC_CLOSE_ALL, 100, 0, 0, 0);
    AutoMove(IDC_CONFERENCE, 100, 0, 0, 0);
    AutoMove(IDC_TRANSFER, 100, 0, 0, 0);
    AutoMove(IDC_HOLD, 100, 0, 0, 0);
    AutoMove(IDC_END, 100, 0, 0, 0);
    AutoMove(IDC_MESSAGES_LIST, 0, 0, 100, 80);
    AutoMove(IDC_MESSAGE, 0, 80, 100, 20);
    lastCall = NULL;

    HICON m_hIcon = theApp.LoadIcon(IDI_MAINFRAME);
    SetIcon(m_hIcon, FALSE);

    int b = MulDiv(16, dpiY, 96);
    imageList.Create(b, b, ILC_COLOR32 | ILC_MASK, 7, 1);
    imageList.Add(LoadImageIcon(IDI_CALL_OUT));
    imageList.Add(LoadImageIcon(IDI_CALL_IN));
    imageList.Add(LoadImageIcon(IDI_CALL_MISS));
    imageList.Add(LoadImageIcon(IDI_ACTIVE));
    imageList.Add(LoadImageIcon(IDI_ACTIVE_SECURE));
    imageList.Add(LoadImageIcon(IDI_CONFERENCE));
    imageList.Add(LoadImageIcon(IDI_CONFERENCE_SECURE));
    imageList.Add(LoadImageIcon(IDI_MESSAGE_IN));
    imageList.Add(LoadImageIcon(IDI_ON_HOLD));
    imageList.Add(LoadImageIcon(IDI_ON_REMOTE_HOLD));
    imageList.Add(LoadImageIcon(IDI_ON_REMOTE_HOLD_CONFERENCE));

    m_messagesTab.SetImageList(&imageList);

    TranslateDialog(this->m_hWnd);

#ifndef _GLOBAL_VIDEO
    GetDlgItem(IDC_VIDEO_CALL)->ShowWindow(SW_HIDE);
#else
    if (accountSettings.disableVideo) {
        GetDlgItem(IDC_VIDEO_CALL)->ShowWindow(SW_HIDE);
    }
#endif

    CFont* font = this->GetFont();
    LOGFONT lf;
    font->GetLogFont(&lf);
    lf.lfHeight = -MulDiv(13, dpiY, 96);
    fontList.CreateFontIndirect(&lf);
    lf.lfHeight = -MulDiv(14, dpiY, 96);
    fontMessage.CreateFontIndirect(&lf);

    CRichEditCtrl* richEditList = (CRichEditCtrl*)GetDlgItem(IDC_MESSAGES_LIST);
    richEditList->SetEventMask(richEditList->GetEventMask() | ENM_MOUSEEVENTS | ENM_LINK);
    richEditList->SetUndoLimit(0);
    richEditList->SetFont(&fontList);
    richEditList->SetAutoURLDetect();
    CRichEditCtrl* richEdit = (CRichEditCtrl*)GetDlgItem(IDC_MESSAGE);
    richEdit->SetEventMask(richEdit->GetEventMask() | ENM_KEYEVENTS);
    richEdit->SetFont(&fontMessage);

    para.cbSize = sizeof(PARAFORMAT2);
    para.dwMask = PFM_STARTINDENT | PFM_LINESPACING | PFM_SPACEBEFORE | PFM_SPACEAFTER;
    para.dxStartIndent = 100;
    para.dySpaceBefore = 100;
    para.dySpaceAfter = 0;
    para.bLineSpacingRule = 5;
    para.dyLineSpacing = 22;

    m_hIconHold = LoadImageIcon(IDI_HOLD, 16, 16);
    m_hIconResume = LoadImageIcon(IDI_RESUME, 16, 16);
    ((CButton*)GetDlgItem(IDC_HOLD))->SetIcon(m_hIconHold);

    MENUITEMINFO mii;
    mii.cbSize = sizeof(MENUITEMINFO);
    mii.fMask = MIIM_SUBMENU;

    CMenu* tracker;

    menuTransfer.LoadMenu(IDR_MENU_CALL_TRANSFER);
    tracker = menuTransfer.GetSubMenu(0);
    TranslateMenu(tracker->m_hMenu);

    menuAttendedTransfer.CreatePopupMenu();

    menuConference.LoadMenu(IDR_MENU_CALL_CONFERENCE);
    tracker = menuConference.GetSubMenu(0);
    TranslateMenu(tracker->m_hMenu);

    menuMerge.CreatePopupMenu();
    mii.hSubMenu = menuMerge.m_hMenu;
    tracker->SetMenuItemInfo(ID_MERGE, &mii);

    m_ButtonCall.m_FaceColor = _GLOBAL_DIALER_CALL_COLOR;
    m_ButtonCall.m_TextColor = RGB(255, 255, 255);
    m_ButtonCall.EnableWindow(m_ButtonCall.IsWindowEnabled());
    m_ButtonEnd.m_FaceColor = _GLOBAL_DIALER_END_COLOR;
    m_ButtonEnd.m_TextColor = RGB(255, 255, 255);
    m_ButtonEnd.EnableWindow(m_ButtonEnd.IsWindowEnabled());

    return TRUE;
}

void MessagesDlg::OnDestroy()
{
    //mainDlg->messagesDlg = nullptr;
    CBaseDialog::OnDestroy();
}

void MessagesDlg::PostNcDestroy()
{
    CBaseDialog::PostNcDestroy();
}

BEGIN_MESSAGE_MAP(MessagesDlg, CBaseDialog)
    ON_WM_CREATE()
    ON_WM_SYSCOMMAND()
    ON_WM_MOVE()
    ON_WM_SIZE()
    ON_WM_DESTROY()
    ON_COMMAND(ID_CLOSEALLTABS, OnCloseAllTabs)
    ON_COMMAND(ID_GOTOLASTTAB, OnGoToLastTab)
    ON_COMMAND(ID_COPY, OnCopy)
    ON_COMMAND(ID_SELECT_ALL, OnSelectAll)
    ON_COMMAND_RANGE(ID_ATTENDED_TRANSFER_RANGE, ID_ATTENDED_TRANSFER_RANGE + 99, OnAttendedTransferRange)
    ON_COMMAND_RANGE(ID_MERGE_RANGE, ID_MERGE_RANGE + 99, OnMerge)
    ON_COMMAND(ID_MERGE_ALL, OnMergeAll)
    ON_COMMAND(IDCANCEL, OnCancel)
    ON_BN_CLICKED(IDOK, &MessagesDlg::OnBnClickedOk)
    ON_NOTIFY(EN_MSGFILTER, IDC_MESSAGE, &MessagesDlg::OnEnMsgfilterMessage)
    ON_NOTIFY(EN_LINK, IDC_MESSAGES_LIST, &MessagesDlg::OnEnLink)
    ON_NOTIFY(TCN_SELCHANGE, IDC_MESSAGES_TAB, &MessagesDlg::OnTcnSelchangeTab)
    ON_NOTIFY(TCN_SELCHANGING, IDC_MESSAGES_TAB, &MessagesDlg::OnTcnSelchangingTab)
    ON_MESSAGE(WM_CONTEXTMENU, OnContextMenu)
    ON_MESSAGE(UM_CLOSETAB, &MessagesDlg::OnCloseTab)
    ON_BN_CLICKED(IDC_CALL_END, &MessagesDlg::OnBnClickedCallEnd)
    ON_BN_CLICKED(IDC_VIDEO_CALL, &MessagesDlg::OnBnClickedVideoCall)
    ON_BN_CLICKED(IDC_TRANSFER, &MessagesDlg::OnBnClickedTransfer)
    ON_BN_CLICKED(IDC_CONFERENCE, &MessagesDlg::OnBnClickedConference)
    ON_COMMAND(ID_TRANSFER, OnTransfer)
    ON_COMMAND(ID_ATTENDED_TRANSFER, OnAttendedTransfer)
    ON_COMMAND(ID_CONFERENCE, OnConference)
    ON_COMMAND(ID_SEPARATE, OnSeparate)
    ON_COMMAND(ID_SEPARATE_ALL, OnSeparateAll)
    ON_COMMAND(ID_DISCONNECT, OnDisconnect)
    ON_BN_CLICKED(IDC_HOLD, &MessagesDlg::OnBnClickedHold)
    ON_BN_CLICKED(IDC_END, &MessagesDlg::OnBnClickedEnd)
    ON_BN_CLICKED(IDC_CLOSE_ALL, &MessagesDlg::OnBnClickedCloseAll)
    ON_BN_CLICKED(IDC_LAST_CALL, &MessagesDlg::OnBnClickedLastCall)
END_MESSAGE_MAP()

void MessagesDlg::OnSysCommand(UINT nID, LPARAM lParam)
{
    if (nID == SC_CLOSE) {
        OnCancel();
        return;
    }
    __super::OnSysCommand(nID, lParam);
}

LRESULT MessagesDlg::OnContextMenu(WPARAM wParam, LPARAM lParam)
{
    int x = GET_X_LPARAM(lParam);
    int y = GET_Y_LPARAM(lParam);
    POINT pt = { x, y };
    RECT rc;
    if (x != -1 || y != -1) {
        ScreenToClient(&pt);
        GetClientRect(&rc);
        if (!PtInRect(&rc, pt)) {
            x = y = -1;
        }
    }
    else {
        ::ClientToScreen((HWND)wParam, &pt);
        x = 10 + pt.x;
        y = 10 + pt.y;
    }
    if (x != -1 || y != -1) {
        CMenu menu;
        menu.LoadMenu(IDR_MENU_TABS);
        CMenu* tracker = menu.GetSubMenu(0);
        TranslateMenu(tracker->m_hMenu);
        tracker->TrackPopupMenu(0, x, y, this);
        return TRUE;
    }
    return DefWindowProc(WM_CONTEXTMENU, wParam, lParam);
}

void MessagesDlg::OnClose()
{
    msip_call_hangup_all_noincoming();
    ShowWindow(SW_HIDE);
    mainDlg->PostMessage(UM_NOTIFYICON, NULL, WM_LBUTTONUP);

}

void MessagesDlg::OnMove(int x, int y)
{
    if (IsWindowVisible() && !IsZoomed() && !IsIconic()) {
        CRect cRect;
        GetWindowRect(&cRect);
        accountSettings.messagesX = cRect.left;
        accountSettings.messagesY = cRect.top;
        mainDlg->AccountSettingsPendingSave();
    }
}

void MessagesDlg::OnSize(UINT type, int w, int h)
{
    CBaseDialog::OnSize(type, w, h);
    if (IsWindowVisible() && type == SIZE_RESTORED) {
        CRect cRect;
        GetWindowRect(&cRect);
        accountSettings.messagesW = cRect.Width();
        accountSettings.messagesH = cRect.Height();
        mainDlg->AccountSettingsPendingSave();
    }
}

void MessagesDlg::OnCancel()
{
    MessagesContact* messagesContact = GetMessageContact();
    if (messagesContact) {
        if (messagesContact->callId != -1) {
            OnBnClickedEnd();
            return;
        }
        OnGoToLastTab();
        MessagesContact* messagesContactLast = GetMessageContact();
        if (messagesContactLast) {
            if (messagesContactLast != messagesContact) {
                return;
            }
        }
    }
    OnClose();
}

void MessagesDlg::OnBnClickedOk()
{
}

void MessagesDlg::SetWindowTitle(const MessagesContact& messagesContact)
{
    if (messagesContact.name == messagesContact.numberShort) {
        SetWindowText(messagesContact.numberShort);
    }
    else {
        CString str;
        str.Format(_T("%s – %s"), messagesContact.name, messagesContact.numberShort);
        SetWindowText(str);
    }
}

MessagesContact* MessagesDlg::AddTab(CString address, BOOL activate, pjsua_call_info* call_info, call_user_data* user_data, BOOL notShowWindow, BOOL ifExists, CString numberOriginal, CString name)
{
    MessagesContact* messagesContact = nullptr;

    SIPURI sipuri;
    ParseCallSIPURI(address, user_data, sipuri);

    //-- incoming call
    if (call_info && call_info->role == PJSIP_ROLE_UAS) {
        //-- fix domain
        if (!sipuri.user.IsEmpty() && accountSettings.accountId) {
            if (MSIP::IsIP(MSIP::RemovePort(sipuri.domain))) {
                sipuri.domain = get_account_domain();
            }
        }
        //--
    }
    //--
    if (accountSettings.accountId && MSIP::RemovePort(get_account_domain()) == MSIP::RemovePort(sipuri.domain)) {
        sipuri.domain = get_account_domain();
    }

    CString aor = (!sipuri.user.IsEmpty() ? sipuri.user + _T("@") : _T("")) + sipuri.domain;
    CString numberShort = !sipuri.user.IsEmpty() ? sipuri.user : sipuri.domain;

    int nCallId = -1;
    int nNumber = -1;
    for (int i = 0; i < m_messagesTab.GetItemCount(); i++)
    {
        MessagesContact* pMessagesContact = GetMessageContact(i);
        if (call_info && call_info->id != -1 && call_info->id == pMessagesContact->callId) {
            nCallId = i;
            messagesContact = pMessagesContact;
        }
        if (pMessagesContact->aor == aor) {
            nNumber = i;
            if (nCallId == -1) {
                messagesContact = pMessagesContact;
            }
        }
    }
    int exists = nNumber;
    bool bNewCall = false;
    bool bUpdateNumber = false;
    if (nCallId != -1 || nNumber != -1) {
        if (nCallId != -1) {
            exists = nCallId;
            if (nNumber != -1 && nNumber != nCallId) {
                CloseTab(nNumber);
                if (nNumber < nCallId) {
                    exists--;
                }
            }
        }
        if (nNumber == -1 || nNumber != nCallId) {
            messagesContact->aor = aor;
            messagesContact->numberShort = numberShort;
            bUpdateNumber = true;
        }
        if (call_info) {
            if (messagesContact->callId != -1) {
                if (messagesContact->callId != call_info->id) {
                    if (call_info->role == PJSIP_ROLE_UAS && (call_info->state == PJSIP_INV_STATE_INCOMING || call_info->state == PJSIP_INV_STATE_EARLY)) {
                        mainDlg->PostMessage(UM_CALL_ANSWER, (WPARAM)call_info->id, -486);
                    }
                    return NULL;
                }
            }
            else {
                bNewCall = true;
            }
        }
    }
    if (exists == -1 || bNewCall || bUpdateNumber) {
        if (name.IsEmpty()) {
            name = mainDlg->GetNameForCall(sipuri, user_data, numberOriginal);
        }
    }
    if (exists == -1) {
        if (ifExists) {
            if (call_info && call_info->state == PJSIP_INV_STATE_DISCONNECTED) {
                if (call_info->role == PJSIP_ROLE_UAS && call_info->last_status == 486) {
                    mainDlg->pageCalls->Add(call_info->call_id, aor + sipuri.suffix, name, L"", MSIP_CALL_TYPE_MISSED, user_data);
                }
            }
            return NULL;
        }
        messagesContact = new MessagesContact();
        messagesContact->aor = aor;
        messagesContact->numberShort = numberShort;
    }
    if (exists == -1 || bNewCall || bUpdateNumber || !name.IsEmpty()) {
        if (messagesContact->name != name) {
            messagesContact->name = name;
        }
        if (m_messagesTab.GetCurSel() == exists) {
            SetWindowTitle(*messagesContact);
        }
    }

    if (!call_info || call_info->state != PJSIP_INV_STATE_DISCONNECTED) {
        if (call_info) {
            messagesContact->callId = call_info->id;
            messagesContact->callIdStr = MSIP::PjToStr(&call_info->call_id);
        }
        if (messagesContact->callId == -1) {
            messagesContact->numberOriginal = numberOriginal;
            messagesContact->aorSuffix = sipuri.suffix;
        }
    }
    CString tabName = messagesContact->name;
    if (user_data) {
        user_data->CS.Lock();
        if (!user_data->diversion.IsEmpty()) {
            tabName = user_data->diversion + _T(" -> ") + tabName;
        }
        user_data->CS.Unlock();
    }
    tabName = _T("   ") + tabName + _T("  ");
    TCITEM item;
    item.pszText = const_cast<LPTSTR>(tabName.GetString());
    item.cchTextMax = 0;
    if (exists == -1) {
        item.mask = TCIF_PARAM | TCIF_TEXT;
        item.lParam = (LPARAM)messagesContact;
        if (!m_messagesTab.GetItemCount()) {
            m_messagesTab.ShowWindow(SW_SHOW);
            GetDlgItem(IDC_LAST_CALL)->EnableWindow(TRUE);
            GetDlgItem(IDC_CLOSE_ALL)->EnableWindow(TRUE);
            GetDlgItem(IDC_MESSAGE)->EnableWindow(!accountSettings.disableMessaging);
        }
        exists = m_messagesTab.InsertItem(m_messagesTab.GetItemCount(), &item);
        if (m_messagesTab.GetCurSel() == exists) {
            OnChangeTab(call_info, user_data);
        }
    }
    else {
        //--
        item.mask = TCIF_TEXT;
        m_messagesTab.SetItem(exists, &item);
        //--
        if (m_messagesTab.GetCurSel() == exists && call_info) {
            UpdateCallButton(messagesContact->callId != -1, call_info, user_data);
        }
    }
    //--update tab icon
    if (call_info) {
        UpdateTabIcon(messagesContact, exists, call_info, user_data);
    }

    //if (m_messagesTab.GetCurSel() != exists && (activate || !IsWindowVisible())) {
    if ((m_messagesTab.GetCurSel() != exists && activate)
        ) {
        LONG_PTR result;
        OnTcnSelchangingTab(NULL, &result);
        m_messagesTab.SetCurSel(exists);
        OnChangeTab(call_info, user_data);
    }
    if (!IsWindowVisible()) {
        bool show = false;
        if (!notShowWindow) {
                show = true;
        }
        if (show) {
            ShowWindow(SW_SHOW);
            CRichEditCtrl* richEdit = (CRichEditCtrl*)GetDlgItem(IDC_MESSAGE);
            GotoDlgCtrl(richEdit);
        }
    }
    return messagesContact;
}

void MessagesDlg::OnChangeTab(pjsua_call_info* p_call_info, call_user_data* user_data, bool init)
{
    //m_messagesTab.HighlightItem(m_messagesTab.GetCurSel(),FALSE);
    MessagesContact* messagesContact = GetMessageContact();
    if (!messagesContact) {
        return;
    }
    pjsua_call_info call_info;
    if (messagesContact->callId != -1) {
        if (!p_call_info) {
            if (is_pjsua_running() && pjsua_call_get_info(messagesContact->callId, &call_info) == PJ_SUCCESS) {
                p_call_info = &call_info;
            }
        }
    }
    if (messagesContact->hasNewMessages) {
        messagesContact->hasNewMessages = false;
        UpdateTabIcon(messagesContact, m_messagesTab.GetCurSel(), p_call_info, user_data);
    }

    SetWindowTitle(*messagesContact);

    if (messagesContact->callId != -1) {
        UpdateCallButton(TRUE, p_call_info, user_data);
        if (accountSettings.singleMode
            && p_call_info && (p_call_info->role == PJSIP_ROLE_UAC ||
                (p_call_info->role == PJSIP_ROLE_UAS &&
                    (p_call_info->state == PJSIP_INV_STATE_CONFIRMED
                        || p_call_info->state == PJSIP_INV_STATE_CONNECTING)
                    ))
            ) {
            if (messagesContact->name == messagesContact->numberShort) {
                mainDlg->pageDialer->SetNumber(messagesContact->numberShort, 1);
            }
            else {
                CString str;
                str.Format(_T("%s – %s"), messagesContact->name, messagesContact->numberShort);
                mainDlg->pageDialer->SetNumber(str, 1);
            }
        }
    }
    else {
        UpdateCallButton();
        if (accountSettings.singleMode) {
            mainDlg->pageDialer->PostMessage(WM_COMMAND, MAKELPARAM(IDC_CLEAR, 0), 0);
        }
    }
    if (init) {
        CRichEditCtrl* richEditList = (CRichEditCtrl*)GetDlgItem(IDC_MESSAGES_LIST);
        CString messages = messagesContact->messages;
        EDITSTREAM es;
        es.dwCookie = (DWORD_PTR)&messages;
        es.pfnCallback = MEditStreamInCallback;
        richEditList->StreamIn(SF_RTF, es);
        richEditList->PostMessage(WM_VSCROLL, SB_BOTTOM, 0);
        CRichEditCtrl* richEdit = (CRichEditCtrl*)GetDlgItem(IDC_MESSAGE);
        richEdit->SetWindowText(messagesContact->message);
        int nEnd = richEdit->GetTextLengthEx(GTL_NUMCHARS);
        richEdit->SetSel(nEnd, nEnd);
    }
}

void MessagesDlg::OnTcnSelchangeTab(NMHDR* pNMHDR, LRESULT* pResult)
{
    OnChangeTab();
    CButton* buttonHold = (CButton*)GetDlgItem(IDC_HOLD);
    WINDOWINFO wndInfo;
    buttonHold->GetWindowInfo(&wndInfo);
    bool isButtonVisisble = wndInfo.dwStyle & WS_VISIBLE;
    if (isButtonVisisble && buttonHold->GetCheck()) {
        OnBnClickedHold();
    }
    * pResult = 0;
}


void MessagesDlg::OnTcnSelchangingTab(NMHDR* pNMHDR, LRESULT* pResult)
{
    CRichEditCtrl* richEdit = (CRichEditCtrl*)GetDlgItem(IDC_MESSAGE);
    CString str;
    int len = richEdit->GetWindowTextLength();
    LPTSTR ptr = str.GetBuffer(len);
    richEdit->GetWindowText(ptr, len + 1);
    str.ReleaseBuffer();

    MessagesContact* messagesContact = GetMessageContact();
    if (messagesContact) {
        messagesContact->message = str;
    }
    * pResult = 0;
}

LRESULT  MessagesDlg::OnCloseTab(WPARAM wParam, LPARAM lParam)
{
    int i = wParam;
    CloseTab(i);
    return TRUE;
}

BOOL MessagesDlg::CloseTab(int i, BOOL safe)
{
    MessagesContact* messagesContact = NULL;
    if (i == -1) {
        i = m_messagesTab.GetCurSel();
    }
    int count = m_messagesTab.GetItemCount();
    if (i < 0 || i >= count) {
        return TRUE;
    }
    messagesContact = GetMessageContact(i);
    if (!messagesContact) {
        return TRUE;
    }
    if (messagesContact->callId != -1) {
        if (safe) {
            return FALSE;
        }
        msip_call_hangup_fast(messagesContact->callId);
        if (count != m_messagesTab.GetItemCount()) {
            return TRUE;
        }
    }
    m_messagesTab.DeleteItem(i);
    count--;
    delete messagesContact;
    if (!count) {
        GetDlgItem(IDC_MESSAGES_LIST)->SetWindowText(_T(""));
        GetDlgItem(IDC_MESSAGE)->SetWindowText(_T(""));
        GetDlgItem(IDC_CALL_END)->ShowWindow(SW_HIDE);
        GetDlgItem(IDC_VIDEO_CALL)->ShowWindow(SW_HIDE);
        m_messagesTab.ShowWindow(SW_HIDE);
        GetDlgItem(IDC_LAST_CALL)->EnableWindow(FALSE);
        GetDlgItem(IDC_CLOSE_ALL)->EnableWindow(FALSE);
        GetDlgItem(IDC_MESSAGE)->EnableWindow(FALSE);
        m_messagesTab.ShowWindow(SW_HIDE);
        if (IsWindowVisible()) {
            OnClose();
        }
    }
    else {
        int curSel = m_messagesTab.GetCurSel();
        m_messagesTab.SetCurSel(curSel < count && curSel != -1 ? curSel : count - 1);
        OnChangeTab();
    }

    return TRUE;
}

/*
 * Perform a call by string number.
 * You can pass initial call parameters via user_data.
 * Deletes user_data if the call fails.
 * Warning: it may end other calls (close tabs and delete related objects)
 */
pjsua_call_id MessagesDlg::PerformCall(CString number, bool hasVideo, pj_status_t* pStatus, call_user_data* user_data)
{
    pjsua_acc_id acc_id = PJSUA_INVALID_ID;
    pj_str_t pj_uri = {};
    do {
        if (!is_pjsua_running()) {
            if (pStatus) {
                *pStatus = PJSIP_ENOTINITIALIZED;
            }
            break;
        }
        if (!SelectSIPAccount(number, acc_id, &pj_uri)) {
            Account dummy;
            if (pStatus) {
                *pStatus = accountSettings.AccountLoad(1, &dummy) ? PJSIP_EAUTHACCDISABLED : PJSIP_EAUTHACCNOTFOUND;
            }
            break;
        }
        if (accountSettings.singleMode) {
            if (!user_data || (!user_data->inConference && !user_data->hidden)) {
                msip_call_hangup_all_noincoming();
            }
        }
#ifdef _GLOBAL_VIDEO
        if (hasVideo) {
            mainDlg->createPreviewWin();
        }
#endif
        msip_set_sound_device(msip_audio_output);

        pjsua_call_setting call_setting;
        pjsua_call_setting_default(&call_setting);
        call_setting.flag = 0;
        call_setting.vid_cnt = hasVideo ? 1 : 0;

        pj_pool_t* pool = nullptr;
        pjsua_msg_data msg_data;
        msip_msg_data_init(pool, acc_id, msg_data);

        if (user_data && user_data->inConference) {
            pjsua_call_id call_ids[PJSUA_MAX_CALLS];
            unsigned calls_count = PJSUA_MAX_CALLS;
            if (pjsua_enum_calls(call_ids, &calls_count) == PJ_SUCCESS) {
                for (unsigned i = 0; i < calls_count; ++i) {
                    call_user_data* user_data_curr = (call_user_data*)pjsua_call_get_user_data(call_ids[i]);
                    if (user_data_curr) {
                        user_data_curr->CS.Lock();
                        if (user_data_curr->inConference) {
                            pjsua_call_info call_info_curr;
                            pjsua_call_get_info(call_ids[i], &call_info_curr);
                            pj_str_t hvalue, hname;
                            hname = pj_str("X-Conf-Call-ID");
                            hvalue = call_info_curr.call_id;
                            if (!pool) {
                                pool = pjsua_pool_create("msip_msg_data", 256, 256);
                            }
                            pjsip_generic_string_hdr* hdr = pjsip_generic_string_hdr_create(pool, &hname, &hvalue);
                            pj_list_push_back(&msg_data.hdr_list, hdr);
                        }
                        user_data_curr->CS.Unlock();
                    }
                }
            }
        }
        pjsua_call_id call_id = PJSUA_INVALID_ID;
        pj_status_t status = pjsua_call_make_call(
            acc_id,
            &pj_uri,
            &call_setting,
            user_data,
            &msg_data,
            &call_id);
        free(pj_uri.ptr);
        pj_pool_safe_release(&pool);
        if (pStatus) {
            *pStatus = status;
        }
        return call_id;
    } while (false);
    if (user_data) {
        delete user_data;
    }
    return PJSUA_INVALID_ID;
}

/*
 * Start a call using the number from the currently selected tab.
 * You can pass initial call parameters via user_data.
 * Deletes user_data if the call fails.
 * Warning: it may end other calls, close tabs and delete related objects
 */
MessagesContact* MessagesDlg::StartCall(bool hasVideo, call_user_data* user_data)
{
    MessagesContact* messagesContact = GetMessageContact();
    if (!messagesContact || messagesContact->callId != -1) {
        if (user_data) {
            delete user_data;
        }
        return nullptr;
    }
    SIPURI sipuri;
    MSIP::ParseSIPURI(messagesContact->aor, sipuri);
    if (!sipuri.user.IsEmpty()) {
        accountSettings.lastCallNumber = sipuri.user;
        if (sipuri.domain != get_account_domain() || !messagesContact->aorSuffix.IsEmpty()) {
            accountSettings.lastCallNumber.AppendFormat(_T("@%s"), sipuri.domain);
        }
    }
    else {
        accountSettings.lastCallNumber = sipuri.domain;
    }
    accountSettings.lastCallNumber.Append(messagesContact->aorSuffix);
    accountSettings.lastCallNumber.Append(messagesContact->commands);
    accountSettings.lastCallHasVideo = hasVideo;

    if (!user_data) {
        user_data = new call_user_data(PJSUA_INVALID_ID);
    }
    else if (user_data->hidden) {
        // just in case, we must never get here
        delete user_data;
        return nullptr;
    }
    user_data->name = messagesContact->name;
    user_data->commands = messagesContact->commands.Mid(1);

    pj_status_t status = PJSIP_EINVALIDREQURI;

    CString address = messagesContact->aor + messagesContact->aorSuffix;
    pjsua_call_id call_id = PerformCall(address, hasVideo, &status, user_data);
    if (call_id != PJSUA_INVALID_ID) {
        messagesContact = nullptr;
        bool ok = false;
        pjsua_call_info call_info;
        if (pjsua_call_get_info(call_id, &call_info) == PJ_SUCCESS) {
            messagesContact = AddTab(address,
                accountSettings.singleMode,
                &call_info, user_data, true, false
            );
            if (messagesContact) {
                messagesContact->callId = call_id;
                UpdateCallButton(TRUE);
                mainDlg->pageCalls->Add(call_info.call_id, messagesContact->numberOriginal, messagesContact->name, messagesContact->commands, MSIP_CALL_TYPE_OUTGOING, user_data);
                ok = true;
            }
        }
        if (!ok) {
            pjsua_call_hangup(call_id, 0, NULL, NULL);
        }
        return messagesContact;
    }
    else {
        user_data = nullptr; // the old object is deleted
        if (status != PJ_ERESOLVE) {
            CString descr;
            CString message = MSIP::GetErrorMessage(status, descr);
            if (message.IsEmpty() || PJ_STATUS_TO_OS(status) == ERROR_INVALID_PARAMETER) {
                message = Translate(_T("Microphone access is blocked. Please check your antivirus settings or security software."));
            }
            else if (message == _T("Undefined external error.")) {
                ShellExecuteW(
                    NULL,
                    L"open",
                    L"ms-settings:privacy-microphone",
                    NULL,
                    NULL,
                    SW_SHOWNORMAL
                );
                message = Translate(_T("Microphone access is turned off. Enable it in Windows Settings."));
            }
            else if (status == PJMEDIA_EAUD_NODEFDEV) {
                message = Translate(_T("Microphone and speakers are required."));
                descr = Translate(_T("No microphone or audio output device was detected. Please connect them to make calls."));
            }
            TaskDialog(mainDlg->GetSafeHwnd(), NULL,
                _T(_GLOBAL_NAME_NICE),
                message,
                descr,
                TDCBF_OK_BUTTON,
                TD_INFORMATION_ICON, NULL);
        }
    }
    return nullptr;
}

void MessagesDlg::OnBnClickedCallEnd()
{
    MessagesContact* messagesContact = GetMessageContact();
    if (!messagesContact) {
        return;
    }
    if (messagesContact->callId == -1) {
        messagesContact = StartCall();
    }
    else {
        mainDlg->onCallAnswer((WPARAM)messagesContact->callId, (LPARAM)0);
    }
}

void MessagesDlg::OnEndCall(pjsua_call_info* call_info, call_user_data* user_data)
{
    SIPURI sipuri;
    ParseCallSIPURI(call_info, user_data, sipuri);

    CString numberLocal = sipuri.user;
    if (numberLocal.IsEmpty()) {
        numberLocal = sipuri.domain;
    }

    CString info;
    bool bError = true;
    bool elsewhere = false;
    if (call_info->state != PJSIP_INV_STATE_DISCONNECTED || call_info->last_status == 200) {
        info = _T("Call Ended");
        bError = false;
    }
    else {
        if (call_info->last_status == 404) {
            info = _T("Incorrect dialing or non-existent number.");
        }
        else if (call_info->last_status == 487) {
            info = _T("Canceled");
            if (user_data) {
                user_data->CS.Lock();
                if (!user_data->reason.IsEmpty()) {
                    info = user_data->reason;
                }
                user_data->CS.Unlock();
                if (info == _T("Call completed elsewhere")) {
                    elsewhere = true;
                }
            }
        }
        else if (call_info->last_status == 488) {
            info = _T("Unsupported codecs, encryption methods, or other media parameters.");
        }
        else {
            CString rab = MSIP::PjToStr(&call_info->last_status_text);
            if (rab.Find(_T("(PJ_ERESOLVE)")) != -1) {
                rab = _T("Cannot get IP address of the called host");
            }
            else if (rab.Find(_T("(WSAECONNREFUSED)")) != -1) {
                rab = _T("Connection refused");
            }
            else if (rab.Find(_T("(WSAETIMEDOUT)")) != -1) {
                rab = _T("Connection timed out");
            }
            if (call_info->acc_id && call_info->last_status >= 500 && call_info->last_status < 600) {
                info.Format(_T("%s (%d). %s"), Translate(_T("Server error")), call_info->last_status, Translate(rab));
            }
            else {
                info = rab;
            }
        }
    }

    MessagesContact* messagesContact = NULL;
    int messagesContactNum = -1;
    for (int i = 0; i < m_messagesTab.GetItemCount(); i++)
    {
        messagesContact = GetMessageContact(i);
        messagesContactNum = i;
        if (messagesContact->callId == call_info->id)
        {
            lastCall = messagesContact;
            messagesContact->callId = -1;
            UpdateTabIcon(messagesContact);
            if (m_messagesTab.GetCurSel() == i) {
                UpdateCallButton(FALSE, call_info, user_data);
            }
            break;
        }
        messagesContact = NULL;
        messagesContactNum = -1;
    }

    CString aor;
    CString name;
    if (messagesContact) {
        aor = messagesContact->aor;
        name = messagesContact->name;
    }
    else {
        aor = (!sipuri.user.IsEmpty() ? sipuri.user + _T("@") : _T("")) + sipuri.domain;
        name = mainDlg->GetNameForCall(sipuri, user_data);
    }
    if (call_info->role == PJSIP_ROLE_UAS && call_info->connect_duration.sec == 0 && call_info->connect_duration.msec == 0) {
        bool ignore = false;
        bool declined = false;
        if (user_data) {
            user_data->CS.Lock();
            ignore = user_data->hidden;
            declined = user_data->hangup;
            user_data->CS.Unlock();
        }
        if (!ignore) {
            mainDlg->pageCalls->Add(call_info->call_id, aor, name, L"", elsewhere ? MSIP_CALL_TYPE_AE : MSIP_CALL_TYPE_MISSED, user_data);
            if (declined) {
                info = _T("Declined");
            }
            else if (elsewhere) {
                info = _T("Answered Elsewhere");
            }
            else if (info.IsEmpty()) {
                info = _T("Missed Call");
            }
        }
    }
    info = Translate(info);

    mainDlg->pageCalls->SetDuration(call_info->call_id, msip_get_duration(&call_info->connect_duration), msip_get_duration(&call_info->total_duration));
    mainDlg->pageCalls->SetInfo(call_info->call_id, info);
    if (user_data) {
        user_data->CS.Lock();
        user_data->duration = msip_get_duration(&call_info->connect_duration);
        user_data->CS.Unlock();
    }

    if (!messagesContact) {
        return;
    }

    if (mainDlg->autoAnswerTimerCallId != PJSUA_INVALID_ID) {
        KillTimer(IDT_TIMER_AUTOANSWER);
        mainDlg->autoAnswerTimerCallId = PJSUA_INVALID_ID;
    }
    if (mainDlg->forwardingTimerCallId != PJSUA_INVALID_ID) {
        KillTimer(IDT_TIMER_FORWARDING);
        mainDlg->forwardingTimerCallId = PJSUA_INVALID_ID;
    }

    msip_conference_leave(call_info, user_data);

    if (user_data) {
        user_data->CS.Lock();
        msip_call_recording_stop(user_data);
        /* Cancel duration timer, if any */
        if (user_data->auto_hangup_timer.id != PJSUA_INVALID_ID) {
            if (is_pjsua_running()) {
                pjsua_cancel_timer(&user_data->auto_hangup_timer);
            }
            user_data->auto_hangup_timer.id = PJSUA_INVALID_ID;
        }
        user_data->CS.Unlock();
    }
    msip_call_deinit_tonegen(call_info->id, user_data);

    if (accountSettings.localDTMF) {
        if (call_info->state != PJSIP_INV_STATE_DISCONNECTED || call_info->last_status == 200) {
            mainDlg->onPlayerPlay(MSIP_SOUND_HANGUP, 0);
        }
    }

    CString message;
    if (bError) {
        CString message;
        message.Format(_T("%s %s"), Translate(_T("Call failed:")), info);
    }
    else {
        message = info;
    }
    AddMessage(messagesContact, message, MSIP_MESSAGE_TYPE_SYSTEM, TRUE);

    if (accountSettings.singleMode) {
        pjsua_call_id current_call_id = mainDlg->CurrentCallId();
        if (current_call_id == call_info->id || current_call_id == -1) {
            mainDlg->pageDialer->Clear(false);
            mainDlg->pageDialer->UpdateCallButton(FALSE, 0);
        }
    }
    else {
        mainDlg->pageDialer->Clear();
    }
    mainDlg->pageDialer->SetName();

#ifdef _GLOBAL_VIDEO
    if (mainDlg->previewWin && !GetCallsCount()) {
        mainDlg->previewWin->PostMessage(WM_CLOSE);
    }
#endif
    mainDlg->UpdateWindowText(_T("-"));

    if (call_info->state == PJSIP_INV_STATE_DISCONNECTED && call_info->last_status != 200) {
        if (accountSettings.singleMode) {
            if (call_info->last_status == 487 || (call_info->role == PJSIP_ROLE_UAS && (call_info->last_status == 486 || call_info->last_status == 600 || call_info->last_status == 603))) {
                //don't show
            }
            else {
                CString title;
                title.Format(_T("%s %s"), Translate(_T("Call failed:")), name);
                mainDlg->BaloonPopup(title, info, NIIF_INFO);
            }
        }
    }

    if (call_info->state == PJSIP_INV_STATE_DISCONNECTED && (call_info->last_status == 486 || call_info->last_status == 600 || call_info->last_status == 603)) {
        //--
        if (!accountSettings.cmdCallBusy.IsEmpty()) {
            CString params = numberLocal;
            MSIP::RunCmd(accountSettings.cmdCallBusy, params);
        }
        //--
    }
    else {
        //--
        if (!accountSettings.cmdCallEnd.IsEmpty()) {
            CString params = numberLocal;
            MSIP::RunCmd(accountSettings.cmdCallEnd, params);
        }
        //--
        //--
    }

    if (accountSettings.singleMode) {
        if (messagesContactNum != -1) {
            bool isActive = m_messagesTab.GetCurSel() == messagesContactNum;
            CloseTab(messagesContactNum, TRUE);
            if (isActive) {
                OnGoToLastTab();
            }
        }
        else {
            OnGoToLastTab();
        }
    }
}

void MessagesDlg::UpdateCallButton(BOOL active, pjsua_call_info* call_info, call_user_data* user_data)
{
    if (active) {
        if (call_info && call_info->role == PJSIP_ROLE_UAS &&
            (call_info->state == PJSIP_INV_STATE_INCOMING || call_info->state == PJSIP_INV_STATE_EARLY)) {
            GetDlgItem(IDC_CALL_END)->ShowWindow(SW_SHOW);
            GetDlgItem(IDC_CALL_END)->SetWindowText(Translate(_T("Answer")));
        }
        else {
            GetDlgItem(IDC_CALL_END)->ShowWindow(SW_HIDE);
        }
    }
    else {
        GetDlgItem(IDC_CALL_END)->SetWindowText(Translate(_T("Call")));
        GetDlgItem(IDC_CALL_END)->ShowWindow(SW_SHOW);
    }
#ifdef _GLOBAL_VIDEO
    if (accountSettings.disableVideo) {
        GetDlgItem(IDC_VIDEO_CALL)->ShowWindow(SW_HIDE);
    }
    else {
        GetDlgItem(IDC_VIDEO_CALL)->ShowWindow(active ? SW_HIDE : SW_SHOW);
    }
#endif

    GetDlgItem(IDC_END)->ShowWindow(!active ? SW_HIDE : SW_SHOW);
    UpdateHoldButton(call_info);
    UpdateRecButton(user_data);
    if (!active) {
        if (mainDlg->transferDlg) {
            mainDlg->transferDlg->OnClose();
        }
        ::SendMessage(m_hWnd, WM_CANCELMODE, 0, 0);
    }
}

void MessagesDlg::UpdateHoldButton(pjsua_call_info* call_info)
{
    MessagesContact* messagesContact = GetMessageContact();
    if (!messagesContact) {
        return;
    }
    bool hasActions = false;
    bool hasHold = false;
    bool onHold = false;
    CButton* buttonTransfer = (CButton*)GetDlgItem(IDC_TRANSFER);
    CButton* buttonConference = (CButton*)GetDlgItem(IDC_CONFERENCE);
    CButton* buttonHold = (CButton*)GetDlgItem(IDC_HOLD);
    CButton* buttonHoldDialer = (CButton*)mainDlg->pageDialer->GetDlgItem(IDC_HOLD);
    CButton* buttonTransferDialer = (CButton*)mainDlg->pageDialer->GetDlgItem(IDC_TRANSFER);
    if (messagesContact->callId != -1 && call_info) {
        if (messagesContact->callId != call_info->id) {
            return;
        }
        if (call_info->state == PJSIP_INV_STATE_EARLY ||
            call_info->state == PJSIP_INV_STATE_CONNECTING ||
            call_info->state == PJSIP_INV_STATE_CONFIRMED) {
            hasActions = true;
            if (call_info->state == PJSIP_INV_STATE_CONFIRMED) {
                hasHold = true;
                if (call_info->media_cnt > 0) {
                    if (call_info->media_status == PJSUA_CALL_MEDIA_LOCAL_HOLD
                        || call_info->media_status == PJSUA_CALL_MEDIA_NONE) {
                        onHold = true;
                    }
                }
            }
        }
    }
    //--
    if (hasActions) {
        buttonTransfer->ShowWindow(SW_SHOW);
        buttonConference->ShowWindow(SW_SHOW);
        buttonTransferDialer->EnableWindow(TRUE);
    }
    else {
        buttonTransfer->ShowWindow(SW_HIDE);
        buttonConference->ShowWindow(SW_HIDE);
        buttonTransferDialer->EnableWindow(FALSE);
    }
    mainDlg->pageDialer->EnableButtonCONF(hasActions);
    //--
    if (hasHold) {
        buttonHold->ShowWindow(SW_SHOW);
        buttonHoldDialer->EnableWindow(TRUE);
    }
    else {
        buttonHold->ShowWindow(SW_HIDE);
        buttonHoldDialer->EnableWindow(FALSE);
    }
    //--
    if (onHold) {
        buttonHold->SetCheck(TRUE);
        buttonHold->SetIcon(m_hIconResume);
        buttonHoldDialer->SetCheck(TRUE);
        buttonHoldDialer->SetIcon(mainDlg->pageDialer->m_hIconResume);
    }
    else {
        buttonHold->SetCheck(FALSE);
        buttonHold->SetIcon(m_hIconHold);
        buttonHoldDialer->SetCheck(FALSE);
        buttonHoldDialer->SetIcon(mainDlg->pageDialer->m_hIconHold);
    }
    //--
}

void MessagesDlg::UpdateRecButton(call_user_data* user_data)
{
    bool state = false;
    MessagesContact* messagesContact = GetMessageContact();
    if (messagesContact) {
        if (messagesContact->callId != -1) {
            if (!user_data) {
                if (is_pjsua_running()) {
                    user_data = (call_user_data*)pjsua_call_get_user_data(messagesContact->callId);
                }
            }
            if (user_data) {
                user_data->CS.Lock();
                if (user_data->recorder_id != PJSUA_INVALID_ID) {
                    state = true;
                }
                user_data->CS.Unlock();
            }
        }
    }
    mainDlg->pageDialer->SetCheckREC(state);
}

bool MessagesDlg::CallCheck()
{
    if (!accountSettings.singleMode || !GetCallsCount())
    {
        MessagesContact* messagesContact = GetMessageContact();
        if (!messagesContact || messagesContact->callId == -1)
        {
            return true;
        }
    }
    else {
        mainDlg->GotoTab(0);
    }
    return false;
}

void MessagesDlg::Call(BOOL hasVideo)
{
    if (CallCheck()) {
        MessagesContact* messagesContact = GetMessageContact();
        call_user_data* user_data = new call_user_data(PJSUA_INVALID_ID);
        messagesContact = StartCall(hasVideo, user_data);
    }
}

void MessagesDlg::AddMessage(MessagesContact* messagesContact, CString message, int type, BOOL blockForeground, CTime* pTime)
{
    CTime tm;
    if (pTime) {
        tm = *pTime;
    }
    else {
        tm = CTime::GetCurrentTime();
    }
    if (type == MSIP_MESSAGE_TYPE_SYSTEM) {
        if (messagesContact->lastSystemMessage == message && messagesContact->lastSystemMessageTime > tm.GetTime() - 2) {
            messagesContact->lastSystemMessageTime = tm;
            return;
        }
        messagesContact->lastSystemMessage = message;
        messagesContact->lastSystemMessageTime = tm;
    }
    else if (!messagesContact->lastSystemMessage.IsEmpty()) {
        messagesContact->lastSystemMessage.Empty();
    }

    if (IsWindowVisible() && !blockForeground) {
        SetForegroundWindow();
    }
    CRichEditCtrl richEdit;
    MessagesContact* messagesContactSelected = GetMessageContact();

    CRichEditCtrl* richEditList = (CRichEditCtrl*)GetDlgItem(IDC_MESSAGES_LIST);
    if (messagesContactSelected != messagesContact) {
        CRect rect;
        rect.left = 0;
        rect.top = 0;
        rect.right = 300;
        rect.bottom = 300;
        richEdit.Create(ES_MULTILINE | ES_READONLY | ES_NUMBER | WS_VSCROLL, rect, this, NULL);
        richEdit.SetFont(&fontList);

        CString messages = messagesContact->messages;
        EDITSTREAM es;
        es.dwCookie = (DWORD_PTR)&messages;
        es.pfnCallback = MEditStreamInCallback;
        richEdit.StreamIn(SF_RTF, es);

        richEditList = &richEdit;
    }

    if (messagesContact->messages.IsEmpty()) {
        richEditList->SetSel(0, -1);
        richEditList->SetParaFormat(para);
    }

    COLORREF color;
    CString name;
    if (type == MSIP_MESSAGE_TYPE_LOCAL) {
        color = RGB(0, 0, 0);
        if (!accountSettings.account.displayName.IsEmpty()) {
            name = accountSettings.account.displayName;
        }
    }
    else if (type == MSIP_MESSAGE_TYPE_REMOTE) {
        color = RGB(21, 101, 206);
        name = messagesContact->name;
        int pos = name.Find(_T(" ("));
        if (pos == -1) {
            pos = name.Find(_T("@"));
        }
        if (pos != -1) {
            name = name.Mid(0, pos);
        }
    }

    int nBegin;
    CHARFORMAT cf;
    CString str;

    CString time = MSIP::FormatDateTime(&tm);

    nBegin = richEditList->GetTextLengthEx(GTL_NUMCHARS);
    richEditList->SetSel(nBegin, nBegin);
    str.Format(_T("[%s]  "), time);
    richEditList->ReplaceSel(str);
    cf.dwMask = CFM_BOLD | CFM_COLOR | CFM_SIZE;
    cf.crTextColor = RGB(131, 131, 131);
    cf.dwEffects = 0;
    cf.yHeight = 160;
    richEditList->SetSel(nBegin, -1);
    richEditList->SetSelectionCharFormat(cf);

    if (type != MSIP_MESSAGE_TYPE_SYSTEM) {
        cf.yHeight = 200;
    }
    if (name.GetLength()) {
        nBegin = richEditList->GetTextLengthEx(GTL_NUMCHARS);
        richEditList->SetSel(nBegin, nBegin);
        richEditList->ReplaceSel(name + _T(": "));
        cf.dwMask = CFM_BOLD | CFM_COLOR | CFM_SIZE;
        cf.crTextColor = color;
        cf.dwEffects = CFE_BOLD;
        richEditList->SetSel(nBegin, -1);
        richEditList->SetSelectionCharFormat(cf);
    }

    nBegin = richEditList->GetTextLengthEx(GTL_NUMCHARS);
    richEditList->SetSel(nBegin, nBegin);
    richEditList->ReplaceSel(message + _T("\r\n"));
    cf.dwMask = CFM_BOLD | CFM_COLOR | CFM_SIZE;

    cf.crTextColor = type == MSIP_MESSAGE_TYPE_SYSTEM ? RGB(131, 131, 131) : color;
    cf.dwEffects = 0;

    richEditList->SetSel(nBegin, -1);
    richEditList->SetSelectionCharFormat(cf);

    int selectedIndex = -1;
    if (messagesContactSelected == messagesContact) {
        richEditList->PostMessage(WM_VSCROLL, SB_BOTTOM, 0);
        selectedIndex = m_messagesTab.GetCurSel();
    }
    else {
        if (type == MSIP_MESSAGE_TYPE_REMOTE) {
            messagesContact->hasNewMessages = true;
            UpdateTabIcon(messagesContact);
        }
        /*
        for (int i = 0; i < m_messagesTab.GetItemCount(); i++) {
            if (messagesContact == GetMessageContact(i))
            {
                m_messagesTab.HighlightItem(i, TRUE);
                break;
            }
        }
        */
    }
    str = _T("");
    EDITSTREAM es;
    es.dwCookie = (DWORD_PTR)&str;
    es.pfnCallback = MEditStreamOutCallback;
    richEditList->StreamOut(SF_RTF, es);
    messagesContact->messages = str;
}

void MessagesDlg::OnEnMsgfilterMessage(NMHDR* pNMHDR, LRESULT* pResult)
{
    MSGFILTER* pMsgFilter = reinterpret_cast<MSGFILTER*>(pNMHDR);

    if (pMsgFilter->msg == WM_CHAR) {
        if (pMsgFilter->wParam == VK_RETURN) {
            CRichEditCtrl* richEdit = (CRichEditCtrl*)GetDlgItem(IDC_MESSAGE);
            CString message;
            int len = richEdit->GetWindowTextLength();
            LPTSTR ptr = message.GetBuffer(len);
            richEdit->GetWindowText(ptr, len + 1);
            message.ReleaseBuffer();
            message.Trim();
            if (message.GetLength()) {
                MessagesContact* messagesContact = GetMessageContact();
                if (SendInstantMessage(messagesContact, message)) {
                    richEdit->SetWindowText(_T(""));
                    GotoDlgCtrl(richEdit);
                    AddMessage(messagesContact, message, MSIP_MESSAGE_TYPE_LOCAL);
                    if (accountSettings.localDTMF) {
                        mainDlg->onPlayerPlay(MSIP_SOUND_MESSAGE_OUT, 0);
                    }
                }
            }
            *pResult = 1;
            return;
        }
    }
    *pResult = 0;
}

void MessagesDlg::OnEnLink(NMHDR* pNMHDR, LRESULT* pResult)
{
    ENLINK* pEnLink = reinterpret_cast<ENLINK*>(pNMHDR);
    if (pEnLink->msg == WM_LBUTTONUP) {
        CRichEditCtrl* richEditList = (CRichEditCtrl*)GetDlgItem(IDC_MESSAGES_LIST);
        CHARRANGE cr;
        richEditList->GetSel(cr);
        richEditList->SetSel(pEnLink->chrg);
        CString url = richEditList->GetSelText();
        richEditList->SetSel(cr);
        MSIP::OpenURL(url);
    }
}

BOOL MessagesDlg::SendInstantMessage(MessagesContact* messagesContact, CString message, CString number)
{
    message.Trim();
    if (message.GetLength()) {
        pjsua_acc_id acc_id;
        pj_str_t pj_uri;
        pj_status_t status;
        if (SelectSIPAccount(messagesContact ? messagesContact->aor + messagesContact->aorSuffix : number, acc_id, &pj_uri)) {
            pj_pool_t* pool = nullptr;
            pjsua_msg_data msg_data;
            msip_msg_data_init(pool, acc_id, msg_data);
            char* buf = MSIP::WideCharToPjStr(message);
            status = pjsua_im_send(acc_id, &pj_uri, NULL, &pj_str(buf), &msg_data, NULL);
            free(pj_uri.ptr);
            free(buf);
            pj_pool_safe_release(&pool);
        }
        else {
            Account dummy;
            status = accountSettings.AccountLoad(1, &dummy) ? PJSIP_EAUTHACCDISABLED : PJSIP_EAUTHACCNOTFOUND;
        }
        if (status != PJ_SUCCESS) {
            if (messagesContact) {
                CString descr;
                CString message = MSIP::GetErrorMessage(status, descr);
                AddMessage(messagesContact, message);
            }
        }
        else {
            return TRUE;
        }
    }
    return FALSE;
}

MessagesContact* MessagesDlg::GetMessageContact(int i)
{
    if (i == -1) {
        i = m_messagesTab.GetCurSel();
    }
    if (i != -1) {
        TCITEM item;
        item.mask = TCIF_PARAM;
        if (m_messagesTab.GetItem(i, &item)) {
            return (MessagesContact*)item.lParam;
        }
    }
    return NULL;
}

MessagesContact* MessagesDlg::GetMessageContactInCall()
{
    MessagesContact* messagesContactActive = NULL;
    for (int i = 0; i < m_messagesTab.GetItemCount(); i++) {
        MessagesContact* messagesContact = GetMessageContact(i);
        if (messagesContact->callId != -1) {
            if (messagesContact->mediaStatus == PJSUA_CALL_MEDIA_ACTIVE || messagesContact->mediaStatus == PJSUA_CALL_MEDIA_REMOTE_HOLD) {
                messagesContactActive = messagesContact;
                break;
            }
            if (messagesContact->mediaStatus == PJSUA_CALL_MEDIA_LOCAL_HOLD || messagesContact->mediaStatus == PJSUA_CALL_MEDIA_NONE) {
                messagesContactActive = messagesContact;
            }
        }
    }
    return messagesContactActive;
}

void MessagesDlg::OnBnClickedVideoCall()
{
    StartCall(true);
}

void MessagesDlg::Merge(pjsua_call_id call_id)
{
    if (!is_pjsua_running()) {
        return;
    }
    MessagesContact* messagesContact = GetMessageContact();
    if (!messagesContact || messagesContact->callId == -1) {
        return;
    }
    if (messagesContact->callId == call_id || !pjsua_call_is_active(call_id)) {
        return;
    }
    call_user_data* user_data;
    user_data = (call_user_data*)pjsua_call_get_user_data(messagesContact->callId);
    if (!user_data) {
        user_data = new call_user_data(messagesContact->callId);
        pjsua_call_set_user_data(messagesContact->callId, user_data);
    }
    user_data->CS.Lock();
    user_data->inConference = true;
    user_data->CS.Unlock();

    user_data = (call_user_data*)pjsua_call_get_user_data(call_id);
    if (!user_data) {
        user_data = new call_user_data(messagesContact->callId);
        pjsua_call_set_user_data(messagesContact->callId, user_data);
    }
    user_data->CS.Lock();
    user_data->inConference = true;
    user_data->CS.Unlock();

    pjsua_call_info call_info;
    if (pjsua_call_get_info(call_id, &call_info) != PJ_SUCCESS) {
        return;
    }
    msip_conference_join(&call_info);
    msip_call_unhold(&call_info);
}

void MessagesDlg::Separate(pjsua_call_id call_id)
{
    if (!is_pjsua_running()) {
        return;
    }
    pjsua_call_info call_info;
    if (pjsua_call_get_info(call_id, &call_info) != PJ_SUCCESS) {
        return;
    }
    msip_conference_leave(&call_info);
    msip_call_unhold(&call_info);
}

bool MessagesDlg::CallAction(int action, CString number, pjsua_call_id forward_call_id)
{
    number.Trim();
    if (action == MSIP_ACTION_FORWARD) {
        if (forward_call_id != PJSUA_INVALID_ID) {
            if (number.IsEmpty()) {
                number = accountSettings.forwardingNumber;
            }
            if (!number.IsEmpty()) {
                CString commands;
                CString numberFormated = FormatNumber(number, &commands);
                pjsua_msg_data msg_data;
                pjsua_msg_data_init(&msg_data);
                pjsip_generic_string_hdr subject;
                pj_str_t hvalue, hname;
                hname = pj_str("Contact");
                char* buf = MSIP::WideCharToPjStr(numberFormated);
                pj_strset2(&hvalue, buf);
                pjsip_generic_string_hdr_init2(&subject, &hname, &hvalue);
                pj_list_push_back(&msg_data.hdr_list, &subject);
                pjsua_call_info call_info;
                if (pjsua_call_get_info(forward_call_id, &call_info) == PJ_SUCCESS) {
                    pjsip_generic_string_hdr diversion;
                    hname = pj_str("Diversion");
                    hvalue = call_info.local_info;
                    pjsip_generic_string_hdr_init2(&diversion, &hname, &hvalue);
                    pj_list_push_back(&msg_data.hdr_list, &diversion);
                }
                pj_status_t status = pjsua_call_answer(forward_call_id, 302, NULL, &msg_data);
                free(buf);
                if (status == PJ_SUCCESS) {
                    return true;
                }
            }
        }
        return false;
    }
    if (!number.IsEmpty()) {
        MessagesContact* messagesContactSelected = mainDlg->messagesDlg->GetMessageContact();
        if (!messagesContactSelected || messagesContactSelected->callId == -1) {
            return false;
        }
        CString commands;
        CString numberFormated = FormatNumber(number, &commands);
        SIPURI sipuri;
        MSIP::ParseSIPURI(numberFormated, sipuri);
        if (messagesContactSelected->aor == sipuri.user + _T("@") + sipuri.domain || (sipuri.user.IsEmpty() && messagesContactSelected->aor == sipuri.domain)) {
            CString title;
            if (action == MSIP_ACTION_TRANSFER) {
                title = Translate(_T("Blind Transfer"));
            }
            else if (action == MSIP_ACTION_ATTENDED_TRANSFER) {
                title = Translate(_T("Attended Transfer"));
            }
            else if (action == MSIP_ACTION_INVITE) {
                title = Translate(_T("Invite to Conference"));
            }
            CString message = Translate(_T("You already have an active call with this number."));
            mainDlg->BaloonPopup(title, message, NIIF_ERROR);
            return false;
        }
        call_user_data* user_data;
        user_data = (call_user_data*)pjsua_call_get_user_data(messagesContactSelected->callId);
        if (action == MSIP_ACTION_TRANSFER || action == MSIP_ACTION_ATTENDED_TRANSFER) {
            bool xfer;
            if (user_data) {
                user_data->CS.Lock();
                xfer = !user_data->inConference;
                user_data->CS.Unlock();
            }
            else {
                xfer = true;
            }
            number = sipuri.user;
            if (xfer) {
                if (action == MSIP_ACTION_ATTENDED_TRANSFER) {
                    bool attendedTransferFC = accountSettings.enableFeatureCodeAT && !accountSettings.featureCodeAT.IsEmpty();
                    CString str = accountSettings.featureCodeAT;
                    if (attendedTransferFC) {
                        str.Append(number);
                        msip_call_dial_dtmf(messagesContactSelected->callId, str, true);
                    }
                    else {
                        if (!accountSettings.singleMode) {
                            mainDlg->MakeCall(number);
                        }
                        else {
                            AfxMessageBox(Translate(_T("Attended transfer is not available in single-call mode. Disable this mode or use PBX feature codes.")));
                        }
                    }
                    return true;
                }
                else {
                    bool blindTransferFC = accountSettings.enableFeatureCodeBT && !accountSettings.featureCodeBT.IsEmpty();
                    CString str = accountSettings.featureCodeBT;
                    if (blindTransferFC) {
                        str.Append(number);
                        msip_call_dial_dtmf(messagesContactSelected->callId, str, true);
                    }
                    else {
                        char* buf = MSIP::WideCharToPjStr(numberFormated);
                        pjsua_call_xfer(messagesContactSelected->callId, &pj_str(buf), NULL);
                        free(buf);
                    }
                }
                return true;
            }
        }
        if (action == MSIP_ACTION_INVITE) {
            if (mainDlg->MessagesOpen(number, true)) {
                MessagesContact* messagesContact = GetMessageContact();
                if (messagesContact && messagesContact->callId == -1) {
                    pjsua_call_info call_info;
                    pjsua_call_get_info(messagesContactSelected->callId, &call_info);
                    msip_call_unhold(&call_info);
                    if (!user_data) {
                        user_data = new call_user_data(messagesContactSelected->callId);
                        pjsua_call_set_user_data(messagesContactSelected->callId, user_data);
                    }
                    call_user_data* user_data_new = new call_user_data(PJSUA_INVALID_ID);
                    user_data_new->inConference = true;
                    messagesContact = StartCall(false, user_data_new);
                    messagesContactSelected = nullptr; // invalidate pointer after StartCall
                    if (messagesContact) {
                        user_data->CS.Lock();
                        user_data->inConference = true;
                        user_data->CS.Unlock();
                        return true;
                    }
                }
            }
        }
    }
    return false;
}

void MessagesDlg::OnBnClickedHold()
{
    MessagesContact* messagesContact = GetMessageContact();
    if (!messagesContact || messagesContact->callId == -1) {
        return;
    }
    pjsua_call_info call_info;
    if (is_pjsua_running() && pjsua_call_get_info(messagesContact->callId, &call_info) == PJ_SUCCESS) {
        if (call_info.state == PJSIP_INV_STATE_CONFIRMED) {
            if (call_info.media_status == PJSUA_CALL_MEDIA_LOCAL_HOLD || call_info.media_status == PJSUA_CALL_MEDIA_NONE) {
                msip_call_unhold(&call_info);
            }
            else {
                msip_call_hold(&call_info);
            }
        }
    }
}

void MessagesDlg::OnBnClickedTransfer()
{
    if (accountSettings.enableFeatureCodeAT
        && !accountSettings.featureCodeAT.IsEmpty()) {
        mainDlg->OpenTransferDlg(this, MSIP_ACTION_TRANSFER);
    }
    else {
        OnBnClickedActions();
    }
}

void MessagesDlg::OnBnClickedConference()
{
    OnBnClickedActions(true);
}

void MessagesDlg::OnBnClickedActions(bool isConference)
{
    if (!is_pjsua_running()) {
        return;
    }
    MessagesContact* messagesContact = GetMessageContact();
    if (!messagesContact || messagesContact->callId == -1) {
        return;
    }
    CMenu* tracker;
    if (isConference) {
        tracker = menuConference.GetSubMenu(0);
    }
    else {
        tracker = menuTransfer.GetSubMenu(0);
    }
    pjsua_call_info call_info;
    if (pjsua_call_get_info(messagesContact->callId, &call_info) != PJ_SUCCESS) {
        return;
    }
    call_user_data* user_data = (call_user_data*)pjsua_call_get_user_data(messagesContact->callId);
    bool inConference = false;
    if (user_data) {
        user_data->CS.Lock();
        if (user_data->inConference) {
            inConference = true;
        }
        user_data->CS.Unlock();
    }
    //-- transfer
    tracker->EnableMenuItem(ID_TRANSFER, !inConference ? 0 : MF_GRAYED);
    tracker->EnableMenuItem(ID_ATTENDED_TRANSFER, !inConference ? 0 : MF_GRAYED);
    //-- attended transfer & merge
    if (!isConference) {
        if (menuAttendedTransfer.GetMenuItemCount()) {
            tracker->DeleteMenu(0, MF_BYPOSITION);
            menuAttendedTransfer.DestroyMenu();
            menuAttendedTransfer.CreateMenu();
        }
    }
    while (menuMerge.DeleteMenu(0, MF_BYPOSITION));
    pjsua_call_id call_ids[PJSUA_MAX_CALLS];
    unsigned calls_count = PJSUA_MAX_CALLS;
    int pos = 0;
    int posMerge = 0;
    pjsua_call_id mergeConferenceAddedId = PJSUA_INVALID_ID;
    if (pjsua_enum_calls(call_ids, &calls_count) == PJ_SUCCESS) {
        for (unsigned i = 0; i < calls_count; ++i) {
            if (call_ids[i] != messagesContact->callId) {
                pjsua_call_info call_info_curr;
                pjsua_call_get_info(call_ids[i], &call_info_curr);
                call_user_data* user_data_curr = (call_user_data*)pjsua_call_get_user_data(call_ids[i]);
                bool inConferenceCurr = false;
                if (user_data_curr) {
                    user_data_curr->CS.Lock();
                    if (user_data_curr->inConference) {
                        inConferenceCurr = true;
                    }
                    user_data_curr->CS.Unlock();
                }
                CString str;
                //--attended transfer
                if (call_info_curr.role == PJSIP_ROLE_UAS || call_info_curr.state == PJSIP_INV_STATE_CONFIRMED) {
                    SIPURI sipuri_curr;
                    ParseCallSIPURI(&call_info_curr, user_data_curr, sipuri_curr);
                    str = !sipuri_curr.name.IsEmpty() ? sipuri_curr.name : (!sipuri_curr.user.IsEmpty() ? sipuri_curr.user : (sipuri_curr.domain));
                    if (!isConference) {
                        if (!inConference && !inConferenceCurr) {
                            menuAttendedTransfer.InsertMenu(pos, MF_BYPOSITION, ID_ATTENDED_TRANSFER_RANGE + pos, str);
                            MENUITEMINFO mii;
                            mii.cbSize = sizeof(MENUITEMINFO);
                            mii.fMask = MIIM_DATA;
                            mii.dwItemData = call_ids[i];
                            menuAttendedTransfer.SetMenuItemInfo(pos, &mii, TRUE);
                            pos++;
                        }
                    }
                }
                //--merge
                if (call_info.state == PJSIP_INV_STATE_CONFIRMED && call_info_curr.state == PJSIP_INV_STATE_CONFIRMED) {
                    if (!inConference || !inConferenceCurr) {
                        if (!inConferenceCurr || mergeConferenceAddedId == PJSUA_INVALID_ID) {
                            MENUITEMINFO mii;
                            mii.cbSize = sizeof(MENUITEMINFO);
                            mii.fMask = MIIM_DATA;
                            mii.dwItemData = call_ids[i];
                            if (inConferenceCurr) {
                                str = Translate(_T("Conference"));
                                mergeConferenceAddedId = call_ids[i];
                            }
                            menuMerge.InsertMenu(posMerge, MF_BYPOSITION, ID_MERGE_RANGE + posMerge, str);
                            menuMerge.SetMenuItemInfo(posMerge, &mii, TRUE);
                            posMerge++;
                        }
                    }
                }
                //--
            }
        }
    }
    if (!isConference) {
        if (menuAttendedTransfer.GetMenuItemCount()) {
            tracker->InsertMenu(0, MF_POPUP | MF_BYPOSITION, (UINT_PTR)menuAttendedTransfer.m_hMenu, Translate(_T("Transfer Call to")));
        }
    }
    if (!inConference && mergeConferenceAddedId != PJSUA_INVALID_ID && menuMerge.GetMenuItemCount() > 1) {
        // only 1 conference allowed, remove all regular calls
        while (menuMerge.DeleteMenu(0, MF_BYPOSITION));
        MENUITEMINFO mii;
        mii.cbSize = sizeof(MENUITEMINFO);
        mii.fMask = MIIM_DATA;
        mii.dwItemData = mergeConferenceAddedId;
        menuMerge.InsertMenu(0, MF_BYPOSITION, ID_MERGE_RANGE, Translate(_T("Conference")));
        menuMerge.SetMenuItemInfo(0, &mii, TRUE);
    }
    tracker->EnableMenuItem(ID_MERGE, menuMerge.GetMenuItemCount() ? 0 : MF_GRAYED);
    tracker->EnableMenuItem(ID_MERGE_ALL, menuMerge.GetMenuItemCount() ? 0 : MF_GRAYED);
    //-- invite in conference
    tracker->EnableMenuItem(ID_CONFERENCE, call_info.state == PJSIP_INV_STATE_CONFIRMED && (inConference || mergeConferenceAddedId == PJSUA_INVALID_ID) ? 0 : MF_GRAYED);
    //-- separate & disconnect

    CString str;
    CString format = _T("%s (%s)");
    MENUITEMINFO mii;
    mii.cbSize = sizeof(MENUITEMINFO);
    mii.fMask = MIIM_STRING;

    str.Format(format, Translate(_T("Separate")), messagesContact->name);
    mii.dwTypeData = const_cast<LPTSTR>(str.GetString());
    tracker->SetMenuItemInfo(ID_SEPARATE, &mii);

    str.Format(format, Translate(_T("Disconnect")), messagesContact->name);
    mii.dwTypeData = const_cast<LPTSTR>(str.GetString());
    tracker->SetMenuItemInfo(ID_DISCONNECT, &mii);

    tracker->EnableMenuItem(ID_SEPARATE, inConference ? 0 : MF_GRAYED);
    tracker->EnableMenuItem(ID_SEPARATE_ALL, inConference ? 0 : MF_GRAYED);
    tracker->EnableMenuItem(ID_DISCONNECT, inConference ? 0 : MF_GRAYED);
    //--
    CPoint point;
    GetCursorPos(&point);
    tracker->TrackPopupMenu(0, point.x, point.y, this);
}

void MessagesDlg::OnTransfer()
{
    mainDlg->OpenTransferDlg(this, MSIP_ACTION_TRANSFER);
}

void MessagesDlg::OnAttendedTransfer()
{
    mainDlg->OpenTransferDlg(this, MSIP_ACTION_ATTENDED_TRANSFER);
}

void MessagesDlg::OnAttendedTransferRange(UINT nID)
{
    MessagesContact* messagesContact = GetMessageContact();
    if (!messagesContact || messagesContact->callId == -1) {
        return;
    }
    int pos = nID - ID_ATTENDED_TRANSFER_RANGE;
    MENUITEMINFO mii;
    mii.cbSize = sizeof(MENUITEMINFO);
    mii.fMask = MIIM_DATA;
    menuAttendedTransfer.GetMenuItemInfo(pos, &mii, TRUE);
    pjsua_call_id call_id = mii.dwItemData;
    if (!pjsua_call_is_active(call_id)) {
        return;
    }
    pjsua_call_xfer_replaces(messagesContact->callId, call_id, 0, 0);
}

void MessagesDlg::OnConference()
{
    mainDlg->OpenTransferDlg(this, MSIP_ACTION_INVITE);
}


void MessagesDlg::OnMerge(UINT nID)
{
    int pos = nID - ID_MERGE_RANGE;
    MENUITEMINFO mii;
    mii.cbSize = sizeof(MENUITEMINFO);
    mii.fMask = MIIM_DATA;
    menuMerge.GetMenuItemInfo(pos, &mii, TRUE);
    pjsua_call_id call_id = mii.dwItemData;
    Merge(call_id);
}

void MessagesDlg::OnMergeAll()
{
    if (!is_pjsua_running()) {
        return;
    }
    MessagesContact* messagesContact = GetMessageContact();
    if (!messagesContact || messagesContact->callId == -1) {
        return;
    }
    pjsua_call_id call_ids[PJSUA_MAX_CALLS];
    unsigned calls_count = PJSUA_MAX_CALLS;
    if (pjsua_enum_calls(call_ids, &calls_count) == PJ_SUCCESS) {
        for (unsigned i = 0; i < calls_count; ++i) {
            if (call_ids[i] != messagesContact->callId) {
                Merge(call_ids[i]);
            }
        }
    }
}

void MessagesDlg::OnSeparate()
{
    MessagesContact* messagesContact = GetMessageContact();
    if (!messagesContact || messagesContact->callId == -1) {
        return;
    }
    Separate(messagesContact->callId);
}

void MessagesDlg::OnSeparateAll()
{
    if (!is_pjsua_running()) {
        return;
    }
    MessagesContact* messagesContact = GetMessageContact();
    if (!messagesContact || messagesContact->callId == -1) {
        return;
    }
    pjsua_call_info call_info;
    pjsua_call_get_info(messagesContact->callId, &call_info);
    msip_call_hold(&call_info);
    pjsua_call_id call_ids[PJSUA_MAX_CALLS];
    unsigned calls_count = PJSUA_MAX_CALLS;
    if (pjsua_enum_calls(call_ids, &calls_count) == PJ_SUCCESS) {
        for (unsigned i = 0; i < calls_count; ++i) {
            if (call_ids[i] != messagesContact->callId) {
                pjsua_call_info call_info;
                if (pjsua_call_get_info(call_ids[i], &call_info) == PJ_SUCCESS) {
                    msip_conference_leave(&call_info);
                }
            }
        }
    }
}

void MessagesDlg::OnDisconnect()
{
    MessagesContact* messagesContact = GetMessageContact();
    if (!messagesContact || messagesContact->callId == -1) {
        return;
    }
    msip_call_hangup_fast(messagesContact->callId);
}

void MessagesDlg::OnBnClickedEnd()
{
    MessagesContact* messagesContact = GetMessageContact();
    if (!messagesContact || messagesContact->callId == -1) {
        return;
    }
    msip_call_end(messagesContact->callId);
}

void MessagesDlg::OnCloseAllTabs()
{
    int i = 0;
    while (i < m_messagesTab.GetItemCount()) {
        if (CloseTab(i, TRUE)) {
            i = 0;
        }
        else {
            i++;
        }
    }
}

void MessagesDlg::OnGoToLastTab()
{
    int i = 0;
    BOOL found = FALSE;
    int lastCallIndex = -1;
    while (i < m_messagesTab.GetItemCount()) {
        MessagesContact* messagesContact = GetMessageContact(i);
        if (messagesContact->callId != -1) {
            found = TRUE;
            if (m_messagesTab.GetCurSel() != i) {
                LONG_PTR result;
                OnTcnSelchangingTab(NULL, &result);
                m_messagesTab.SetCurSel(i);
                OnChangeTab();
                break;
            }
        }
        if (messagesContact == lastCall) {
            lastCallIndex = i;
        }
        i++;
    }
    if (!found && lastCallIndex != -1) {
        if (m_messagesTab.GetCurSel() != lastCallIndex) {
            LONG_PTR result;
            OnTcnSelchangingTab(NULL, &result);
            m_messagesTab.SetCurSel(lastCallIndex);
            OnChangeTab();
        }
    }
}

int MessagesDlg::GetCallDuration(pjsua_call_id* call_id)
{
    int duration = -1;
    pjsua_call_info call_info;
    int i = 0;
    int count = 0;
    while (i < m_messagesTab.GetItemCount()) {
        MessagesContact* messagesContact = GetMessageContact(i);
        if (messagesContact->callId != -1) {
            if (is_pjsua_running() && pjsua_call_get_info(messagesContact->callId, &call_info) == PJ_SUCCESS) {
                if (call_info.state == PJSIP_INV_STATE_CONFIRMED) {
                    duration = msip_get_duration(&call_info.connect_duration);
                    *call_id = messagesContact->callId;
                    count++;
                }
            }
        }
        i++;
    }
    if (count > 1) {
        *call_id = PJSUA_INVALID_ID;
        duration = count;
    }
    return duration;
}

int MessagesDlg::GetCallsCount(bool withIncoming, bool onlyActive)
{
    int count = 0;
    int i = 0;
    while (i < m_messagesTab.GetItemCount()) {
        MessagesContact* messagesContact = GetMessageContact(i);
        if (messagesContact->callId != -1) {
            if (onlyActive) {
                if (messagesContact->mediaStatus == PJSUA_CALL_MEDIA_ACTIVE) {
                    count++;
                }
            }
            else {
                if (withIncoming) {
                    count++;
                }
                else {
                    pjsua_call_info call_info;
                    if (is_pjsua_running() && pjsua_call_get_info(messagesContact->callId, &call_info) == PJ_SUCCESS) {
                        if (call_info.role != PJSIP_ROLE_UAS || (call_info.state != PJSIP_INV_STATE_INCOMING && call_info.state != PJSIP_INV_STATE_EARLY)) {
                            count++;
                        }
                    }
                }
            }
        }
        i++;
    }
    return count;
}

void MessagesDlg::OnCopy()
{
    CRichEditCtrl* richEditList = (CRichEditCtrl*)GetDlgItem(IDC_MESSAGES_LIST);
    richEditList->Copy();
}

void MessagesDlg::OnSelectAll()
{
    CRichEditCtrl* richEditList = (CRichEditCtrl*)GetDlgItem(IDC_MESSAGES_LIST);
    richEditList->SetSel(0, -1);
}

void MessagesDlg::OnBnClickedCloseAll()
{
    OnCloseAllTabs();
}

void MessagesDlg::OnBnClickedLastCall()
{
    OnGoToLastTab();
}

void MessagesDlg::UpdateTabIcon(MessagesContact* messagesContact, int tabIndex, pjsua_call_info* p_call_info, call_user_data* user_data)
{
    if (tabIndex == -1) {
        for (int i = 0; i < m_messagesTab.GetItemCount(); i++) {
            if (messagesContact == GetMessageContact(i)) {
                tabIndex = i;
                break;
            }
        }
    }
    if (tabIndex == -1) {
        return;
    }
    int icon = -1;
    if (messagesContact->hasNewMessages) {
        icon = MSIP_TAB_ICON_MESSAGE_IN;
    }
    else if (p_call_info) {
        //-----------------
        switch (p_call_info->state) {
        case PJSIP_INV_STATE_NULL:
        case PJSIP_INV_STATE_DISCONNECTED:
            if (p_call_info->role == PJSIP_ROLE_UAS && !p_call_info->connect_duration.sec && !p_call_info->connect_duration.msec) {
                icon = MSIP_TAB_ICON_CALL_MISS;
            }
            break;
        case PJSIP_INV_STATE_CONFIRMED:
            if (p_call_info->media_status == PJSUA_CALL_MEDIA_LOCAL_HOLD || p_call_info->media_status == PJSUA_CALL_MEDIA_NONE) {
                icon = MSIP_TAB_ICON_ON_HOLD;
            }
            else {
                if (!user_data) {
                    if (is_pjsua_running()) {
                        user_data = (call_user_data*)pjsua_call_get_user_data(p_call_info->id);
                    }
                }
                if (user_data) {
                    user_data->CS.Lock();
                    if (p_call_info->media_status == PJSUA_CALL_MEDIA_REMOTE_HOLD) {
                        if (user_data && user_data->inConference) {
                            icon = MSIP_TAB_ICON_ON_REMOTE_HOLD_CONFERENCE;
                        }
                        else {
                            icon = MSIP_TAB_ICON_ON_REMOTE_HOLD;
                        }
                    }
                    else {
                        if (user_data->inConference) {
                            if (user_data->srtp == MSIP_SRTP) {
                                icon = MSIP_TAB_ICON_CONFERENCE_SECURE;
                            }
                            else {
                                icon = MSIP_TAB_ICON_CONFERENCE;
                            }
                        }
                        else {
                            if (user_data->srtp == MSIP_SRTP) {
                                icon = MSIP_TAB_ICON_ACTIVE_SECURE;
                            }
                            else {
                                icon = MSIP_TAB_ICON_ACTIVE;
                            }
                        }
                    }
                    user_data->CS.Unlock();
                }
                else {
                    if (p_call_info->media_status == PJSUA_CALL_MEDIA_REMOTE_HOLD) {
                        icon = MSIP_TAB_ICON_ON_REMOTE_HOLD;
                    }
                    else {
                        icon = MSIP_TAB_ICON_ACTIVE;
                    }
                }
            }
            break;
        default:
            if (p_call_info->role == PJSIP_ROLE_UAS) {
                icon = MSIP_TAB_ICON_CALL_IN;
            }
            else {
                icon = MSIP_TAB_ICON_CALL_OUT;
            }
            break;
        }
        //-----------------
    }
    TCITEM item;
    item.mask = TCIF_IMAGE;
    item.iImage = icon;
    m_messagesTab.SetItem(tabIndex, &item);
}

bool MessagesDlg::GotoTab(int i) {
    int nTab = m_messagesTab.GetCurSel();
    if (i < 0) {
        int max = m_messagesTab.GetItemCount() - 1;
        if (i == -1) {
            i = nTab < max ? nTab + 1 : 0;
        }
        else {
            i = nTab == 0 ? max : nTab - 1;
        }
    }
    if (nTab != i) {
        TC_ITEM tci;
        tci.mask = TCIF_PARAM;
        if (m_messagesTab.GetItem(i, &tci) && tci.lParam < 0) {
            i = 0;
        }
        if (nTab != i) {
            LRESULT pResult;
            OnTcnSelchangingTab(NULL, &pResult);
            m_messagesTab.SetCurSel(i);
            OnTcnSelchangeTab(NULL, &pResult);
            return true;
        }
    }
    return false;
}

void MessagesDlg::TabIconUpdate(pjsua_call_id call_id)
{
    for (int i = 0; i < m_messagesTab.GetItemCount(); i++) {
        MessagesContact* messagesContact = GetMessageContact(i);
        if (messagesContact->callId == call_id) {
            pjsua_call_info call_info;
            if (pjsua_call_get_info(call_id, &call_info) == PJ_SUCCESS) {
                UpdateTabIcon(messagesContact, i, &call_info);
            }
            break;
        }
    }
}
